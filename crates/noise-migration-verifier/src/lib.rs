use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use noise_core::{
    AccountVault, EncryptedBlob, GroupDeletion, InviteRecord, InviteRotation, MlsControlLog,
    MlsEpochRecord, MlsGroupGenesis, MlsJoinRequest, MlsRemovalRequest, SignedEvent,
    direct_mailbox_id,
};
use noise_transport::SignedRelayDescriptor;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

const REPORT_VERSION: u32 = 1;
const MAX_SHARD_BYTES: u64 = 2_000_000;

#[derive(Clone, Debug)]
pub struct SourceInput {
    pub label: String,
    pub database_path: PathBuf,
    pub media_root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerificationReport {
    pub report_version: u32,
    pub status: &'static str,
    pub primary_source: String,
    pub sources: Vec<SourceReport>,
    pub immutable_unions: Vec<ImmutableUnionReport>,
    pub accounts: AccountMergeReport,
    pub events: EventMergeReport,
    pub media: MediaMergeReport,
    pub push: PushMergeReport,
    pub blockers: Vec<InvariantCount>,
    pub warnings: Vec<InvariantCount>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceReport {
    pub label: String,
    pub database_bytes: u64,
    pub database_sha256: String,
    pub sqlite_integrity: &'static str,
    pub schema_sha256: String,
    pub objects: Vec<SourceObjectReport>,
    pub relay_descriptors: u64,
    pub valid_relay_descriptors: u64,
    pub shard_rows: u64,
    pub shard_bytes: u64,
    pub shard_tombstones: u64,
    pub pending_shard_deletions: u64,
    pub shard_files: u64,
    pub missing_shard_files: u64,
    pub invalid_shard_files: u64,
    pub orphan_shard_files: u64,
    pub push_subscriptions: u64,
    pub push_deliveries: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceObjectReport {
    pub kind: String,
    pub rows: u64,
    pub valid: u64,
    pub invalid: u64,
    pub valid_id_set_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImmutableUnionReport {
    pub kind: String,
    pub unique_ids: u64,
    pub shared_ids: u64,
    pub source_only_ids: u64,
    pub identical_shared_payloads: u64,
    pub conflicting_shared_payloads: u64,
    pub union_id_set_sha256: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AccountMergeReport {
    pub unique_locators: u64,
    pub selected_active: u64,
    pub selected_deleted: u64,
    pub differing_source_revisions: u64,
    pub same_revision_conflicts: u64,
    pub identity_conflicts: u64,
    pub identities_with_multiple_locators: u64,
    pub minimum_selected_revision: Option<u64>,
    pub maximum_selected_revision: Option<u64>,
    pub locator_set_sha256: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EventMergeReport {
    pub raw_unique_events: u64,
    pub group_events: u64,
    pub canonical_group_events: u64,
    pub direct_mailbox_copies: u64,
    pub canonical_direct_events: u64,
    pub collapsed_direct_copies: u64,
    pub unresolved_sender_only_direct_events: u64,
    pub conflicting_direct_recipients: u64,
    pub duplicate_direct_receiver_copies: u64,
    pub duplicate_direct_sender_copies: u64,
    pub shared_legacy_group_sequence_duplicates: u64,
    pub cross_source_group_sequence_conflicts: u64,
    pub canonical_event_count: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MediaMergeReport {
    pub provider_scoped_live_shards: u64,
    pub provider_scoped_tombstones: u64,
    pub referenced_ciphertext_bytes: u64,
    pub unique_payloads: u64,
    pub unique_payload_bytes: u64,
    pub duplicate_payload_references: u64,
    pub canonical_encrypted_objects: u64,
    pub canonical_storage_bytes: u64,
    pub legacy_json_encrypted_objects: u64,
    pub canonicalizable_references: u64,
    pub noncanonical_payloads: u64,
    pub conflicting_canonical_object_ids: u64,
    pub cross_source_shared_shard_ids: u64,
    pub missing_files: u64,
    pub invalid_files: u64,
    pub orphan_files: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PushMergeReport {
    pub selected_source: String,
    pub selected_subscriptions: u64,
    pub selected_deliveries: u64,
    pub non_primary_subscriptions: u64,
    pub non_primary_deliveries: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct InvariantCount {
    pub code: String,
    pub count: u64,
}

#[derive(Clone)]
struct StoredObject {
    raw_payload: Vec<u8>,
    meta: ObjectMeta,
}

#[derive(Clone)]
enum ObjectMeta {
    Account(AccountVault),
    Event(SignedEvent),
    MlsGenesis(MlsGroupGenesis),
    MlsEpoch(MlsEpochRecord),
    Other,
}

struct SourceData {
    report: SourceReport,
    objects: BTreeMap<String, BTreeMap<String, StoredObject>>,
    shards: BTreeMap<String, ShardRow>,
    tombstones: BTreeSet<String>,
}

#[derive(Clone)]
struct ShardRow {
    payload_hash: String,
    byte_length: u64,
    file_state: ShardFileState,
    canonical_object_id: Option<String>,
    canonical_storage_hash: Option<String>,
    canonical_storage_length: Option<u64>,
    legacy_json_encoding: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShardFileState {
    Valid,
    Missing,
    Invalid,
}

#[derive(Default)]
struct Findings {
    blockers: BTreeMap<String, u64>,
    warnings: BTreeMap<String, u64>,
}

impl Findings {
    fn block(&mut self, code: impl Into<String>, count: u64) {
        if count > 0 {
            *self.blockers.entry(code.into()).or_default() += count;
        }
    }

    fn warn(&mut self, code: impl Into<String>, count: u64) {
        if count > 0 {
            *self.warnings.entry(code.into()).or_default() += count;
        }
    }
}

pub fn verify(
    mut inputs: Vec<SourceInput>,
    primary_source: &str,
) -> anyhow::Result<VerificationReport> {
    inputs.sort_by(|left, right| left.label.cmp(&right.label));
    if inputs.len() < 2 {
        bail!("at least two relay snapshots are required");
    }
    let mut labels = BTreeSet::new();
    for input in &inputs {
        validate_label(&input.label)?;
        if !labels.insert(input.label.as_str()) {
            bail!("source labels must be unique");
        }
    }
    if !labels.contains(primary_source) {
        bail!("primary source does not match a configured source");
    }

    let mut findings = Findings::default();
    let mut sources = Vec::with_capacity(inputs.len());
    for input in &inputs {
        sources.push(read_source(input, &mut findings)?);
    }

    let accounts = merge_accounts(&sources, &mut findings);
    let immutable_unions = merge_immutable_objects(&sources, &mut findings);
    verify_mls_logs(&sources, &mut findings);
    let events = reconcile_events(&sources, &accounts.selected, &mut findings);
    let media = reconcile_media(&sources, &mut findings);
    let push = reconcile_push(&sources, primary_source, &mut findings);

    let status = if findings.blockers.is_empty() {
        "pass"
    } else {
        "blocked"
    };
    Ok(VerificationReport {
        report_version: REPORT_VERSION,
        status,
        primary_source: primary_source.to_owned(),
        sources: sources.into_iter().map(|source| source.report).collect(),
        immutable_unions,
        accounts: accounts.report,
        events,
        media,
        push,
        blockers: findings
            .blockers
            .into_iter()
            .map(|(code, count)| InvariantCount { code, count })
            .collect(),
        warnings: findings
            .warnings
            .into_iter()
            .map(|(code, count)| InvariantCount { code, count })
            .collect(),
    })
}

struct MergedAccounts {
    report: AccountMergeReport,
    selected: BTreeMap<String, AccountVault>,
}

fn read_source(input: &SourceInput, findings: &mut Findings) -> anyhow::Result<SourceData> {
    let database_metadata = fs::metadata(&input.database_path).with_context(|| {
        format!(
            "source {} database is unavailable",
            sanitized_label(&input.label)
        )
    })?;
    if !database_metadata.is_file() {
        bail!("source database is not a regular file");
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", input.database_path.display(), suffix));
        if sidecar.exists() {
            bail!(
                "source {} is not a clean snapshot: SQLite sidecar is present",
                sanitized_label(&input.label)
            );
        }
    }
    let database_hash_before = sha256_file(&input.database_path)?;
    let database_uri = immutable_sqlite_uri(&input.database_path)?;
    let connection = Connection::open_with_flags(
        database_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| {
        format!(
            "could not open source {} read-only",
            sanitized_label(&input.label)
        )
    })?;
    connection.pragma_update(None, "query_only", true)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!(
            "source {} failed SQLite integrity verification",
            sanitized_label(&input.label)
        );
    }
    for table in [
        "relay_objects",
        "relay_directory",
        "relay_shards",
        "relay_shard_delete_queue",
        "relay_shard_tombstones",
        "push_subscriptions",
        "push_deliveries",
    ] {
        if !table_exists(&connection, table)? {
            bail!(
                "source {} is missing a required relay table",
                sanitized_label(&input.label)
            );
        }
    }
    let schema_sha256 = schema_digest(&connection)?;

    let mut objects: BTreeMap<String, BTreeMap<String, StoredObject>> = BTreeMap::new();
    let mut object_totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT kind, object_id, payload
         FROM relay_objects
         ORDER BY kind, object_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let stored_kind: String = row.get(0)?;
        let object_id: String = row.get(1)?;
        let payload: String = row.get(2)?;
        let kind = if known_object_kind(&stored_kind) {
            stored_kind.as_str()
        } else {
            "unknown"
        };
        let totals = object_totals.entry(kind.to_owned()).or_default();
        totals.0 += 1;
        match verify_object(&stored_kind, &object_id, payload.as_bytes()) {
            Ok(meta) => {
                totals.1 += 1;
                objects.entry(kind.to_owned()).or_default().insert(
                    object_id,
                    StoredObject {
                        raw_payload: payload.into_bytes(),
                        meta,
                    },
                );
            }
            Err(code) => findings.block(format!("object_{kind}_{code}"), 1),
        }
    }
    drop(rows);
    drop(statement);

    let object_reports = object_totals
        .into_iter()
        .map(|(kind, (rows, valid))| {
            let valid_ids = objects
                .get(&kind)
                .map(|entries| entries.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            SourceObjectReport {
                kind,
                rows,
                valid,
                invalid: rows - valid,
                valid_id_set_sha256: digest_ids(valid_ids.iter().map(String::as_str)),
            }
        })
        .collect();

    let (relay_descriptors, valid_relay_descriptors) = verify_relay_descriptors(&connection)?;
    if relay_descriptors != valid_relay_descriptors {
        findings.block(
            "invalid_relay_descriptor",
            relay_descriptors - valid_relay_descriptors,
        );
    }
    let pending_deletions = read_id_set(&connection, "relay_shard_delete_queue", "shard_id")?;
    let tombstones = read_id_set(&connection, "relay_shard_tombstones", "shard_id")?;
    let mut shards = read_shards(&connection, findings)?;
    let file_inventory =
        verify_shard_files(&input.media_root, &mut shards, findings, &input.label)?;
    let shard_bytes = shards.values().map(|shard| shard.byte_length).sum();
    let push_subscriptions = count_rows(&connection, "push_subscriptions")?;
    let push_deliveries = count_rows(&connection, "push_deliveries")?;
    verify_push_rows(&connection, findings)?;

    let live_and_tombstoned = shards
        .keys()
        .filter(|shard_id| tombstones.contains(*shard_id))
        .count() as u64;
    findings.block("live_shard_is_tombstoned", live_and_tombstoned);
    let unqueued_tombstones = pending_deletions
        .iter()
        .filter(|shard_id| !tombstones.contains(*shard_id))
        .count() as u64;
    findings.block("shard_delete_queue_without_tombstone", unqueued_tombstones);
    for id in tombstones.iter().chain(pending_deletions.iter()) {
        if !valid_hex_id(id) {
            findings.block("invalid_shard_tombstone_id", 1);
        }
    }

    drop(connection);
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", input.database_path.display(), suffix));
        if sidecar.exists() {
            bail!(
                "source {} produced a SQLite sidecar during verification",
                sanitized_label(&input.label)
            );
        }
    }
    let database_hash_after = sha256_file(&input.database_path)?;
    if database_hash_before != database_hash_after
        || database_metadata.len()
            != fs::metadata(&input.database_path)
                .context("source database disappeared during verification")?
                .len()
    {
        bail!(
            "source {} changed during verification",
            sanitized_label(&input.label)
        );
    }

    Ok(SourceData {
        report: SourceReport {
            label: input.label.clone(),
            database_bytes: database_metadata.len(),
            database_sha256: database_hash_before,
            sqlite_integrity: "ok",
            schema_sha256,
            objects: object_reports,
            relay_descriptors,
            valid_relay_descriptors,
            shard_rows: shards.len() as u64,
            shard_bytes,
            shard_tombstones: tombstones.len() as u64,
            pending_shard_deletions: pending_deletions.len() as u64,
            shard_files: file_inventory.files,
            missing_shard_files: file_inventory.missing,
            invalid_shard_files: file_inventory.invalid,
            orphan_shard_files: file_inventory.orphans,
            push_subscriptions,
            push_deliveries,
        },
        objects,
        shards,
        tombstones,
    })
}

fn immutable_sqlite_uri(path: &Path) -> anyhow::Result<String> {
    let path = path
        .to_str()
        .context("SQLite snapshot path must be valid UTF-8")?;
    let mut uri = String::from("file:");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut uri, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    uri.push_str("?immutable=1");
    Ok(uri)
}

fn verify_object(kind: &str, object_id: &str, payload: &[u8]) -> Result<ObjectMeta, &'static str> {
    if !valid_hex_id(object_id) {
        return Err("noncanonical_id");
    }
    match kind {
        "account" => {
            let value: AccountVault =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.locator != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Account(value))
        }
        "event" => {
            let value: SignedEvent = serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.event_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Event(value))
        }
        "invite" => {
            let value: InviteRecord =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.locator != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        "invite_rotation" => {
            let value: InviteRotation =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.group_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        "deletion" => {
            let value: GroupDeletion =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.group_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        "mls_join_request" => {
            let value: MlsJoinRequest =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.request_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        "mls_removal_request" => {
            let value: MlsRemovalRequest =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.request_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        "mls_genesis" => {
            let value: MlsGroupGenesis =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.group_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::MlsGenesis(value))
        }
        "mls_epoch" => {
            let value: MlsEpochRecord =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_signature")?;
            if value.record_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::MlsEpoch(value))
        }
        "blob" => {
            let value: EncryptedBlob =
                serde_json::from_slice(payload).map_err(|_| "invalid_json")?;
            value.verify().map_err(|_| "invalid_payload")?;
            if value.blob_id != object_id {
                return Err("id_mismatch");
            }
            Ok(ObjectMeta::Other)
        }
        _ => Err("unknown_kind"),
    }
}

fn merge_accounts(sources: &[SourceData], findings: &mut Findings) -> MergedAccounts {
    let mut by_locator: BTreeMap<String, Vec<&StoredObject>> = BTreeMap::new();
    for source in sources {
        if let Some(accounts) = source.objects.get("account") {
            for (locator, account) in accounts {
                by_locator.entry(locator.clone()).or_default().push(account);
            }
        }
    }
    let mut selected = BTreeMap::new();
    let mut report = AccountMergeReport {
        unique_locators: by_locator.len() as u64,
        locator_set_sha256: digest_ids(by_locator.keys().map(String::as_str)),
        ..AccountMergeReport::default()
    };
    for (locator, records) in by_locator {
        let mut identity_keys = BTreeSet::new();
        let mut revisions: BTreeMap<u64, BTreeSet<&[u8]>> = BTreeMap::new();
        let mut candidates = Vec::new();
        for record in records {
            let ObjectMeta::Account(account) = &record.meta else {
                continue;
            };
            identity_keys.insert(account.identity_public_key.as_str());
            revisions
                .entry(account.revision)
                .or_default()
                .insert(record.raw_payload.as_slice());
            candidates.push(account.clone());
        }
        if identity_keys.len() > 1 {
            report.identity_conflicts += 1;
            findings.block("account_identity_conflict", 1);
            continue;
        }
        let same_revision_conflicts = revisions
            .values()
            .filter(|payloads| payloads.len() > 1)
            .count() as u64;
        report.same_revision_conflicts += same_revision_conflicts;
        findings.block("account_same_revision_conflict", same_revision_conflicts);
        if revisions.len() > 1 {
            report.differing_source_revisions += 1;
        }
        candidates.sort_by_key(|account| account.revision);
        if let Some(account) = candidates.pop() {
            report.minimum_selected_revision = Some(
                report
                    .minimum_selected_revision
                    .map_or(account.revision, |value| value.min(account.revision)),
            );
            report.maximum_selected_revision = Some(
                report
                    .maximum_selected_revision
                    .map_or(account.revision, |value| value.max(account.revision)),
            );
            if account.deleted {
                report.selected_deleted += 1;
            } else {
                report.selected_active += 1;
            }
            selected.insert(locator, account);
        }
    }
    let mut identity_locators: BTreeMap<&str, u64> = BTreeMap::new();
    for account in selected.values() {
        *identity_locators
            .entry(account.identity_public_key.as_str())
            .or_default() += 1;
    }
    report.identities_with_multiple_locators = identity_locators
        .values()
        .filter(|count| **count > 1)
        .count() as u64;
    findings.warn(
        "identity_has_multiple_recovery_locators",
        report.identities_with_multiple_locators,
    );
    MergedAccounts { report, selected }
}

fn merge_immutable_objects(
    sources: &[SourceData],
    findings: &mut Findings,
) -> Vec<ImmutableUnionReport> {
    let kinds = [
        "blob",
        "deletion",
        "event",
        "invite",
        "invite_rotation",
        "mls_epoch",
        "mls_genesis",
        "mls_join_request",
        "mls_removal_request",
    ];
    kinds
        .into_iter()
        .map(|kind| {
            let mut by_id: BTreeMap<&str, Vec<&[u8]>> = BTreeMap::new();
            for source in sources {
                if let Some(objects) = source.objects.get(kind) {
                    for (id, object) in objects {
                        by_id
                            .entry(id.as_str())
                            .or_default()
                            .push(object.raw_payload.as_slice());
                    }
                }
            }
            let shared_ids = by_id.values().filter(|payloads| payloads.len() > 1).count() as u64;
            let conflicting_shared_payloads = by_id
                .values()
                .filter(|payloads| {
                    payloads.len() > 1
                        && payloads
                            .iter()
                            .skip(1)
                            .any(|payload| *payload != payloads[0])
                })
                .count() as u64;
            findings.block(
                format!("immutable_{kind}_payload_conflict"),
                conflicting_shared_payloads,
            );
            ImmutableUnionReport {
                kind: kind.to_owned(),
                unique_ids: by_id.len() as u64,
                shared_ids,
                source_only_ids: by_id
                    .values()
                    .filter(|payloads| payloads.len() == 1)
                    .count() as u64,
                identical_shared_payloads: shared_ids - conflicting_shared_payloads,
                conflicting_shared_payloads,
                union_id_set_sha256: digest_ids(by_id.keys().copied()),
            }
        })
        .collect()
}

fn union_objects<'a>(sources: &'a [SourceData], kind: &str) -> BTreeMap<&'a str, &'a StoredObject> {
    let mut union = BTreeMap::new();
    for source in sources {
        if let Some(objects) = source.objects.get(kind) {
            for (id, object) in objects {
                union.entry(id.as_str()).or_insert(object);
            }
        }
    }
    union
}

fn verify_mls_logs(sources: &[SourceData], findings: &mut Findings) {
    let geneses = union_objects(sources, "mls_genesis");
    let epochs = union_objects(sources, "mls_epoch");
    let mut epochs_by_group: BTreeMap<&str, Vec<MlsEpochRecord>> = BTreeMap::new();
    for object in epochs.values() {
        let ObjectMeta::MlsEpoch(epoch) = &object.meta else {
            continue;
        };
        epochs_by_group
            .entry(epoch.bundle.group_id.as_str())
            .or_default()
            .push(epoch.clone());
    }
    for values in epochs_by_group.values_mut() {
        values.sort_by_key(|epoch| epoch.bundle.epoch);
    }
    for object in geneses.values() {
        let ObjectMeta::MlsGenesis(genesis) = &object.meta else {
            continue;
        };
        let log = MlsControlLog {
            genesis: genesis.clone(),
            epochs: epochs_by_group
                .remove(genesis.group_id.as_str())
                .unwrap_or_default(),
        };
        if log.verify().is_err() {
            findings.block("invalid_mls_control_log", 1);
        }
    }
    findings.block(
        "mls_epoch_without_genesis",
        epochs_by_group.values().map(Vec::len).sum::<usize>() as u64,
    );
}

fn reconcile_events(
    sources: &[SourceData],
    accounts: &BTreeMap<String, AccountVault>,
    findings: &mut Findings,
) -> EventMergeReport {
    let events = union_objects(sources, "event");
    let mut mailbox_owners = BTreeMap::new();
    let identities: BTreeSet<_> = accounts
        .values()
        .map(|account| account.identity_public_key.as_str())
        .collect();
    for identity_public_key in identities {
        match direct_mailbox_id(identity_public_key) {
            Ok(mailbox_id) => {
                if let Some(existing_identity) = mailbox_owners.get(&mailbox_id) {
                    if *existing_identity != identity_public_key {
                        findings.block("direct_mailbox_identity_collision", 1);
                    }
                } else {
                    mailbox_owners.insert(mailbox_id, identity_public_key);
                }
            }
            Err(_) => findings.block("account_has_invalid_identity_key", 1),
        }
    }

    let mut event_source_counts: BTreeMap<&str, u64> = BTreeMap::new();
    for source in sources {
        if let Some(source_events) = source.objects.get("event") {
            for event_id in source_events.keys() {
                *event_source_counts.entry(event_id.as_str()).or_default() += 1;
            }
        }
    }

    let mut report = EventMergeReport {
        raw_unique_events: events.len() as u64,
        ..EventMergeReport::default()
    };
    let mut direct: BTreeMap<(&str, u64), Vec<(&SignedEvent, &str)>> = BTreeMap::new();
    let mut group_sequences: BTreeMap<(&str, &str, u64), Vec<&str>> = BTreeMap::new();
    for (event_id, object) in &events {
        let ObjectMeta::Event(event) = &object.meta else {
            continue;
        };
        if let Some(owner) = mailbox_owners.get(&event.group_id) {
            report.direct_mailbox_copies += 1;
            direct
                .entry((event.author_public_key.as_str(), event.author_sequence))
                .or_default()
                .push((event, owner));
        } else {
            report.group_events += 1;
            group_sequences
                .entry((
                    event.group_id.as_str(),
                    event.author_public_key.as_str(),
                    event.author_sequence,
                ))
                .or_default()
                .push(event_id);
        }
    }
    for event_ids in group_sequences
        .values()
        .filter(|event_ids| event_ids.len() > 1)
    {
        let duplicates = event_ids.len().saturating_sub(1) as u64;
        let shared_by_every_source = event_ids.iter().all(|event_id| {
            event_source_counts
                .get(event_id)
                .copied()
                .unwrap_or_default()
                == sources.len() as u64
        });
        if shared_by_every_source {
            report.shared_legacy_group_sequence_duplicates += duplicates;
            findings.warn("shared_legacy_group_sequence_duplicate", duplicates);
        } else {
            report.cross_source_group_sequence_conflicts += duplicates;
            findings.block("cross_source_group_sequence_conflict", duplicates);
        }
    }
    let group_sequence_duplicates = report.shared_legacy_group_sequence_duplicates
        + report.cross_source_group_sequence_conflicts;
    report.canonical_group_events = report.group_events - group_sequence_duplicates;

    for ((author, _), copies) in direct {
        let receiver_copies: Vec<_> = copies
            .iter()
            .filter(|(_, mailbox_owner)| *mailbox_owner != author)
            .collect();
        let sender_copies = copies.len() - receiver_copies.len();
        let recipients: BTreeSet<_> = receiver_copies
            .iter()
            .map(|(_, mailbox_owner)| *mailbox_owner)
            .collect();
        if recipients.is_empty() {
            report.unresolved_sender_only_direct_events += 1;
            findings.block("direct_sender_copy_has_no_receiver_copy", 1);
            continue;
        }
        if recipients.len() > 1 {
            report.conflicting_direct_recipients += 1;
            findings.block("direct_event_has_conflicting_recipients", 1);
            continue;
        }
        if receiver_copies.len() > 1 {
            report.duplicate_direct_receiver_copies += (receiver_copies.len() - 1) as u64;
            findings.block(
                "direct_event_has_duplicate_receiver_copies",
                (receiver_copies.len() - 1) as u64,
            );
        }
        if sender_copies > 1 {
            report.duplicate_direct_sender_copies += (sender_copies - 1) as u64;
            findings.block(
                "direct_event_has_duplicate_sender_copies",
                (sender_copies - 1) as u64,
            );
        }
        report.canonical_direct_events += 1;
        report.collapsed_direct_copies += copies.len().saturating_sub(1) as u64;
    }
    report.canonical_event_count = report.canonical_group_events + report.canonical_direct_events;
    report
}

fn reconcile_media(sources: &[SourceData], findings: &mut Findings) -> MediaMergeReport {
    let mut report = MediaMergeReport::default();
    let mut payloads = BTreeMap::<(&str, u64), u64>::new();
    let mut valid_payloads = BTreeSet::<(&str, u64)>::new();
    let mut canonical_payloads = BTreeMap::<(&str, u64), (&str, &str, u64)>::new();
    let mut legacy_json_object_ids = BTreeSet::<&str>::new();
    let mut shard_sources = BTreeMap::<&str, u64>::new();
    for source in sources {
        report.provider_scoped_live_shards += source.shards.len() as u64;
        report.provider_scoped_tombstones += source.tombstones.len() as u64;
        for (shard_id, shard) in &source.shards {
            report.referenced_ciphertext_bytes += shard.byte_length;
            *payloads
                .entry((shard.payload_hash.as_str(), shard.byte_length))
                .or_default() += 1;
            *shard_sources.entry(shard_id.as_str()).or_default() += 1;
            match shard.file_state {
                ShardFileState::Valid => {
                    let payload_key = (shard.payload_hash.as_str(), shard.byte_length);
                    valid_payloads.insert(payload_key);
                    if let (Some(object_id), Some(storage_hash), Some(storage_length)) = (
                        shard.canonical_object_id.as_deref(),
                        shard.canonical_storage_hash.as_deref(),
                        shard.canonical_storage_length,
                    ) {
                        report.canonicalizable_references += 1;
                        if shard.legacy_json_encoding {
                            legacy_json_object_ids.insert(object_id);
                        }
                        if let Some(existing) = canonical_payloads
                            .insert(payload_key, (object_id, storage_hash, storage_length))
                            && existing != (object_id, storage_hash, storage_length)
                        {
                            findings.block("canonical_payload_object_id_conflict", 1);
                        }
                    }
                }
                ShardFileState::Missing => report.missing_files += 1,
                ShardFileState::Invalid => report.invalid_files += 1,
            }
        }
        report.orphan_files += source.report.orphan_shard_files;
    }
    report.unique_payloads = payloads.len() as u64;
    report.unique_payload_bytes = payloads.keys().map(|(_, length)| *length).sum();
    report.duplicate_payload_references = payloads.values().map(|count| count - 1).sum();
    report.noncanonical_payloads = valid_payloads
        .iter()
        .filter(|payload| !canonical_payloads.contains_key(*payload))
        .count() as u64;
    let mut object_payloads = BTreeMap::<&str, BTreeSet<(&str, u64)>>::new();
    for (_, (object_id, storage_hash, storage_length)) in canonical_payloads {
        object_payloads
            .entry(object_id)
            .or_default()
            .insert((storage_hash, storage_length));
    }
    report.canonical_encrypted_objects = object_payloads.len() as u64;
    report.canonical_storage_bytes = object_payloads
        .values()
        .filter_map(|payloads| payloads.first())
        .map(|(_, length)| *length)
        .sum();
    report.legacy_json_encrypted_objects = legacy_json_object_ids.len() as u64;
    report.conflicting_canonical_object_ids = object_payloads
        .values()
        .filter(|payloads| payloads.len() > 1)
        .count() as u64;
    report.cross_source_shared_shard_ids = shard_sources
        .values()
        .filter(|sources| **sources > 1)
        .count() as u64;
    findings.warn(
        "cross_source_shard_ids_are_provider_namespaced",
        report.cross_source_shared_shard_ids,
    );
    findings.block("missing_referenced_shard_file", report.missing_files);
    findings.block("invalid_referenced_shard_file", report.invalid_files);
    findings.block(
        "legacy_payload_is_not_complete_encrypted_object",
        report.noncanonical_payloads,
    );
    findings.block(
        "canonical_object_id_has_conflicting_payloads",
        report.conflicting_canonical_object_ids,
    );
    findings.block("unclassified_orphan_shard_file", report.orphan_files);
    report
}

fn reconcile_push(
    sources: &[SourceData],
    primary_source: &str,
    findings: &mut Findings,
) -> PushMergeReport {
    let mut report = PushMergeReport {
        selected_source: primary_source.to_owned(),
        ..PushMergeReport::default()
    };
    for source in sources {
        if source.report.label == primary_source {
            report.selected_subscriptions = source.report.push_subscriptions;
            report.selected_deliveries = source.report.push_deliveries;
        } else {
            report.non_primary_subscriptions += source.report.push_subscriptions;
            report.non_primary_deliveries += source.report.push_deliveries;
        }
    }
    findings.warn(
        "non_primary_push_state_excluded",
        report.non_primary_subscriptions + report.non_primary_deliveries,
    );
    report
}

fn read_shards(
    connection: &Connection,
    findings: &mut Findings,
) -> anyhow::Result<BTreeMap<String, ShardRow>> {
    let mut shards = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT shard_id, payload_hash, delete_token_hash, byte_length
         FROM relay_shards
         ORDER BY shard_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (shard_id, payload_hash, delete_token_hash, byte_length) = row?;
        if !valid_hex_id(&shard_id)
            || !valid_hex_id(&payload_hash)
            || !valid_hex_id(&delete_token_hash)
            || !(1..=MAX_SHARD_BYTES as i64).contains(&byte_length)
        {
            findings.block("invalid_shard_metadata", 1);
            continue;
        }
        shards.insert(
            shard_id,
            ShardRow {
                payload_hash,
                byte_length: byte_length as u64,
                file_state: ShardFileState::Missing,
                canonical_object_id: None,
                canonical_storage_hash: None,
                canonical_storage_length: None,
                legacy_json_encoding: false,
            },
        );
    }
    Ok(shards)
}

#[derive(Default)]
struct FileInventory {
    files: u64,
    missing: u64,
    invalid: u64,
    orphans: u64,
}

fn verify_shard_files(
    media_root: &Path,
    shards: &mut BTreeMap<String, ShardRow>,
    findings: &mut Findings,
    source_label: &str,
) -> anyhow::Result<FileInventory> {
    if !media_root.is_dir() {
        bail!(
            "source {} media root is unavailable",
            sanitized_label(source_label)
        );
    }
    let shard_root = media_root.join("shards");
    if !shard_root.is_dir() {
        bail!(
            "source {} media root has no shards directory",
            sanitized_label(source_label)
        );
    }
    let mut inventory = FileInventory::default();
    let mut file_ids = BTreeSet::new();
    for prefix_entry in fs::read_dir(&shard_root)? {
        let prefix_entry = prefix_entry?;
        if !prefix_entry.file_type()?.is_dir() {
            inventory.invalid += 1;
            continue;
        }
        let prefix = prefix_entry.file_name();
        let prefix = prefix.to_string_lossy();
        if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            inventory.invalid += 1;
            continue;
        }
        for file_entry in fs::read_dir(prefix_entry.path())? {
            let file_entry = file_entry?;
            if !file_entry.file_type()?.is_file() {
                inventory.invalid += 1;
                continue;
            }
            inventory.files += 1;
            let file_name = file_entry.file_name();
            let file_name = file_name.to_string_lossy();
            let Some(shard_id) = file_name.strip_suffix(".bin") else {
                inventory.invalid += 1;
                continue;
            };
            if !valid_hex_id(shard_id) || &shard_id[..2] != prefix.as_ref() {
                inventory.invalid += 1;
                continue;
            }
            file_ids.insert(shard_id.to_owned());
        }
    }
    for (shard_id, shard) in shards.iter_mut() {
        if !file_ids.contains(shard_id) {
            inventory.missing += 1;
            shard.file_state = ShardFileState::Missing;
            continue;
        }
        let path = shard_root
            .join(&shard_id[..2])
            .join(format!("{shard_id}.bin"));
        let payload = fs::read(&path)?;
        let valid = payload.len() as u64 == shard.byte_length
            && blake3::hash(&payload).to_hex().as_str() == shard.payload_hash;
        if valid {
            shard.file_state = ShardFileState::Valid;
            if let Some((blob, legacy_json_encoding)) = canonical_encrypted_blob(&payload) {
                let storage_bytes = blob.storage_bytes()?;
                shard.canonical_storage_hash =
                    Some(blake3::hash(&storage_bytes).to_hex().to_string());
                shard.canonical_storage_length = Some(storage_bytes.len() as u64);
                shard.canonical_object_id = Some(blob.blob_id);
                shard.legacy_json_encoding = legacy_json_encoding;
            }
        } else {
            inventory.invalid += 1;
            shard.file_state = ShardFileState::Invalid;
        }
    }
    inventory.orphans = file_ids
        .iter()
        .filter(|shard_id| !shards.contains_key(*shard_id))
        .count() as u64;
    findings.block("invalid_media_directory_entry", inventory.invalid);
    Ok(inventory)
}

fn canonical_encrypted_blob(payload: &[u8]) -> Option<(EncryptedBlob, bool)> {
    if let Ok(blob) = EncryptedBlob::from_storage_bytes(payload) {
        return Some((blob, false));
    }
    let blob = serde_json::from_slice::<EncryptedBlob>(payload).ok()?;
    blob.verify().ok()?;
    (serde_json::to_vec(&blob).ok()?.as_slice() == payload).then_some((blob, true))
}

fn verify_relay_descriptors(connection: &Connection) -> anyhow::Result<(u64, u64)> {
    let mut total = 0;
    let mut valid = 0;
    let mut statement =
        connection.prepare("SELECT relay_id, descriptor FROM relay_directory ORDER BY relay_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        total += 1;
        let (relay_id, payload) = row?;
        let Ok(descriptor) = serde_json::from_str::<SignedRelayDescriptor>(&payload) else {
            continue;
        };
        if descriptor.relay_id == relay_id
            && descriptor
                .verify_at(descriptor.issued_at_unix_seconds)
                .is_ok()
        {
            valid += 1;
        }
    }
    Ok((total, valid))
}

fn verify_push_rows(connection: &Connection, findings: &mut Findings) -> anyhow::Result<()> {
    let invalid_subscriptions: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM push_subscriptions
         WHERE mailbox_id = ''
            OR public_key = ''
            OR installation_id = ''
            OR device_token = ''
            OR environment NOT IN ('production', 'sandbox')
            OR topic = ''
            OR updated_at_millis < 0",
        [],
        |row| row.get(0),
    )?;
    findings.block(
        "invalid_push_subscription",
        invalid_subscriptions.try_into().unwrap_or(u64::MAX),
    );
    let invalid_deliveries: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM push_deliveries
         WHERE mailbox_id = ''
            OR installation_id = ''
            OR event_id = ''
            OR delivered_at_millis < 0",
        [],
        |row| row.get(0),
    )?;
    findings.block(
        "invalid_push_delivery",
        invalid_deliveries.try_into().unwrap_or(u64::MAX),
    );
    Ok(())
}

fn read_id_set(
    connection: &Connection,
    table: &str,
    column: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let sql = format!("SELECT {column} FROM {table} ORDER BY {column}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect::<Result<BTreeSet<String>, _>>()
        .map_err(Into::into)
}

fn count_rows(connection: &Connection, table: &str) -> anyhow::Result<u64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    count.try_into().context("stored row count is invalid")
}

fn table_exists(connection: &Connection, table: &str) -> anyhow::Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table' AND name = ?1
             )",
            [table],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn schema_digest(connection: &Connection) -> anyhow::Result<String> {
    let mut statement = connection.prepare(
        "SELECT type, name, COALESCE(sql, '')
         FROM sqlite_master
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    for row in rows {
        let (kind, name, sql) = row?;
        for value in [kind, name, sql] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut reader = BufReader::new(
        fs::File::open(path).with_context(|| "could not open snapshot for hashing")?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn digest_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for id in ids {
        hasher.update(id.as_bytes());
    }
    hex_digest(hasher.finalize().as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn valid_hex_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn known_object_kind(kind: &str) -> bool {
    matches!(
        kind,
        "account"
            | "blob"
            | "deletion"
            | "event"
            | "invite"
            | "invite_rotation"
            | "mls_epoch"
            | "mls_genesis"
            | "mls_join_request"
            | "mls_removal_request"
    )
}

fn validate_label(label: &str) -> anyhow::Result<()> {
    if label.is_empty()
        || label.len() > 64
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
    {
        return Err(anyhow!("source label is invalid"));
    }
    Ok(())
}

fn sanitized_label(label: &str) -> &str {
    if validate_label(label).is_ok() {
        label
    } else {
        "invalid"
    }
}
