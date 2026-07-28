use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use futures_util::{StreamExt, stream};
use noise_core::{
    AccountVault, GroupDeletion, InviteRecord, InviteRotation, MlsEpochRecord, MlsGroupGenesis,
    MlsJoinRequest, MlsRemovalReason, MlsRemovalRequest, SignedEvent, direct_mailbox_id,
    direct_scope_id,
};
use noise_migration_verifier::{
    LegacyPayloadEncoding, SourceInput, normalize_legacy_media_payload, verify,
};
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, PutMode, PutOptions,
    aws::AmazonS3Builder, local::LocalFileSystem, path::Path as ObjectPath,
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use tokio_postgres::{NoTls, Transaction};
use url::Url;

const MAX_UPLOAD_CONCURRENCY: usize = 16;

#[derive(Clone)]
pub struct ImportSource {
    pub input: SourceInput,
    pub compatibility_origin: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportSummary {
    pub status: &'static str,
    pub providers: u64,
    pub canonical_objects: u64,
    pub canonical_storage_bytes: u64,
    pub live_legacy_aliases: u64,
    pub deleted_legacy_aliases: u64,
    pub legacy_json_aliases: u64,
    pub accounts: u64,
    pub account_locators: u64,
    pub groups: u64,
    pub events: u64,
    pub direct_events: u64,
    pub invitations: u64,
    pub invite_rotations: u64,
    pub group_deletions: u64,
}

pub struct ImportPlan {
    pub summary: ImportSummary,
    objects: Vec<ObjectPlan>,
    aliases: Vec<AliasPlan>,
    providers: BTreeMap<String, String>,
    social: SocialPlan,
}

#[derive(Clone)]
struct ObjectPlan {
    object_id: String,
    storage_key: String,
    storage_hash: String,
    storage_length: u64,
    source_path: PathBuf,
}

struct AliasPlan {
    provider: String,
    shard_id: Vec<u8>,
    state: &'static str,
    payload_hash: Option<Vec<u8>>,
    ciphertext_length: Option<i64>,
    canonical_object_id: Option<Vec<u8>>,
    payload_encoding: Option<&'static str>,
    deletion_capability_hash: Option<Vec<u8>>,
}

struct SocialPlan {
    accounts: Vec<AccountVault>,
    events: Vec<SignedEvent>,
    direct_event_ids: BTreeSet<String>,
    invitations: Vec<InviteRecord>,
    invite_rotations: Vec<InviteRotation>,
    group_deletions: Vec<GroupDeletion>,
    mls_geneses: Vec<MlsGroupGenesis>,
    mls_epochs: Vec<MlsEpochRecord>,
    mls_join_requests: Vec<MlsJoinRequest>,
    mls_removal_requests: Vec<MlsRemovalRequest>,
}

pub fn build_plan(sources: Vec<ImportSource>, primary_source: &str) -> anyhow::Result<ImportPlan> {
    let verifier_inputs = sources
        .iter()
        .map(|source| source.input.clone())
        .collect::<Vec<_>>();
    let verification = verify(verifier_inputs, primary_source)?;
    if verification.status != "pass" {
        bail!("migration verifier did not pass");
    }
    let social = build_social_plan(&sources)?;

    let mut providers = BTreeMap::new();
    let mut objects = BTreeMap::<String, ObjectPlan>::new();
    let mut aliases = Vec::new();
    let mut legacy_json_aliases = 0_u64;
    let mut deleted_aliases = 0_u64;

    for source in &sources {
        let origin = canonical_compatibility_origin(&source.compatibility_origin)?;
        if providers
            .insert(source.input.label.clone(), origin)
            .is_some()
        {
            bail!("import source labels must be unique");
        }
        let database = open_snapshot(&source.input.database_path)?;
        let tombstones = read_id_set(&database, "relay_shard_tombstones")?;
        let mut statement = database.prepare(
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
            let decoded_shard_id = decode_hex_32(&shard_id)?;
            if tombstones.contains(&shard_id) {
                bail!("live shard is also tombstoned");
            }
            let source_path = source
                .input
                .media_root
                .join("shards")
                .join(&shard_id[..2])
                .join(format!("{shard_id}.bin"));
            let payload = fs::read(&source_path).with_context(|| {
                format!(
                    "verified media file for source {} is unavailable",
                    source.input.label
                )
            })?;
            if payload.len() as i64 != byte_length
                || blake3::hash(&payload).to_hex().as_str() != payload_hash
            {
                bail!("media changed after verification");
            }
            let normalized = normalize_legacy_media_payload(&payload)?;
            let storage_hash = blake3::hash(&normalized.storage_bytes).to_hex().to_string();
            let storage_length = normalized.storage_bytes.len() as u64;
            let storage_key = canonical_storage_key(&normalized.object_id)?;
            let candidate = ObjectPlan {
                object_id: normalized.object_id.clone(),
                storage_key,
                storage_hash: storage_hash.clone(),
                storage_length,
                source_path: source_path.clone(),
            };
            if let Some(existing) = objects.get_mut(&normalized.object_id) {
                if existing.storage_hash != storage_hash
                    || existing.storage_length != storage_length
                {
                    bail!("canonical media object has conflicting normalized bytes");
                }
                if normalized.source_encoding == LegacyPayloadEncoding::Nsb2 {
                    existing.source_path = source_path;
                }
            } else {
                objects.insert(normalized.object_id.clone(), candidate);
            }
            let payload_encoding = match normalized.source_encoding {
                LegacyPayloadEncoding::Nsb2 => "nsb2",
                LegacyPayloadEncoding::LegacyJson => {
                    legacy_json_aliases += 1;
                    "legacy_json"
                }
            };
            aliases.push(AliasPlan {
                provider: source.input.label.clone(),
                shard_id: decoded_shard_id,
                state: "live",
                payload_hash: Some(decode_hex_32(&payload_hash)?),
                ciphertext_length: Some(byte_length),
                canonical_object_id: Some(decode_hex_32(&normalized.object_id)?),
                payload_encoding: Some(payload_encoding),
                deletion_capability_hash: Some(decode_hex_32(&delete_token_hash)?),
            });
        }
        drop(statement);
        for shard_id in tombstones {
            deleted_aliases += 1;
            aliases.push(AliasPlan {
                provider: source.input.label.clone(),
                shard_id: decode_hex_32(&shard_id)?,
                state: "deleted",
                payload_hash: None,
                ciphertext_length: None,
                canonical_object_id: None,
                payload_encoding: None,
                deletion_capability_hash: None,
            });
        }
    }

    let canonical_storage_bytes = objects.values().map(|object| object.storage_length).sum();
    let summary = ImportSummary {
        status: "ready",
        providers: providers.len() as u64,
        canonical_objects: objects.len() as u64,
        canonical_storage_bytes,
        live_legacy_aliases: aliases.iter().filter(|alias| alias.state == "live").count() as u64,
        deleted_legacy_aliases: deleted_aliases,
        legacy_json_aliases,
        accounts: social
            .accounts
            .iter()
            .map(|account| account.identity_public_key.as_str())
            .collect::<BTreeSet<_>>()
            .len() as u64,
        account_locators: social.accounts.len() as u64,
        groups: social
            .mls_geneses
            .iter()
            .map(|genesis| genesis.group_id.as_str())
            .chain(
                social
                    .group_deletions
                    .iter()
                    .map(|deletion| deletion.group_id.as_str()),
            )
            .chain(
                social
                    .events
                    .iter()
                    .filter(|event| !social.direct_event_ids.contains(&event.event_id))
                    .map(|event| event.group_id.as_str()),
            )
            .collect::<BTreeSet<_>>()
            .len() as u64,
        events: social.events.len() as u64,
        direct_events: social.direct_event_ids.len() as u64,
        invitations: social.invitations.len() as u64,
        invite_rotations: social.invite_rotations.len() as u64,
        group_deletions: social.group_deletions.len() as u64,
    };
    if summary.canonical_objects != verification.media.canonical_encrypted_objects
        || summary.canonical_storage_bytes != verification.media.canonical_storage_bytes
        || summary.live_legacy_aliases != verification.media.provider_scoped_live_shards
        || summary.deleted_legacy_aliases != verification.media.provider_scoped_tombstones
    {
        bail!("import plan does not reproduce the passing verifier report");
    }

    Ok(ImportPlan {
        summary,
        objects: objects.into_values().collect(),
        aliases,
        providers,
        social,
    })
}

fn build_social_plan(sources: &[ImportSource]) -> anyhow::Result<SocialPlan> {
    let mut objects = BTreeMap::<String, BTreeMap<String, Vec<u8>>>::new();
    let mut account_candidates = BTreeMap::<String, Vec<AccountVault>>::new();
    for source in sources {
        let database = open_snapshot(&source.input.database_path)?;
        let mut statement = database.prepare(
            "SELECT kind, object_id, payload
             FROM relay_objects
             ORDER BY kind, object_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?.into_bytes(),
            ))
        })?;
        for row in rows {
            let (kind, object_id, payload) = row?;
            if kind == "account" {
                let account: AccountVault = serde_json::from_slice(&payload)?;
                account.verify()?;
                if account.locator != object_id {
                    bail!("verified account locator changed during import planning");
                }
                account_candidates
                    .entry(object_id)
                    .or_default()
                    .push(account);
                continue;
            }
            let by_id = objects.entry(kind).or_default();
            if let Some(existing) = by_id.get(&object_id) {
                if existing != &payload {
                    bail!("verified immutable object changed during import planning");
                }
            } else {
                by_id.insert(object_id, payload);
            }
        }
    }

    let mut accounts = Vec::with_capacity(account_candidates.len());
    for (locator, mut candidates) in account_candidates {
        candidates.sort_by_key(|account| account.revision);
        let selected = candidates
            .pop()
            .context("verified account candidate disappeared")?;
        if selected.locator != locator {
            bail!("verified account selection changed during import planning");
        }
        accounts.push(selected);
    }
    accounts.sort_by(|left, right| left.locator.cmp(&right.locator));

    let identities = accounts
        .iter()
        .map(|account| account.identity_public_key.clone())
        .collect::<BTreeSet<_>>();
    let mut mailbox_owners = BTreeMap::new();
    for identity in &identities {
        mailbox_owners.insert(direct_mailbox_id(identity)?, identity.clone());
    }

    let raw_events = parse_objects::<SignedEvent>(&objects, "event")?;
    let mut group_events = BTreeMap::<(String, String, u64), Vec<SignedEvent>>::new();
    let mut direct_events = BTreeMap::<(String, u64), Vec<SignedEvent>>::new();
    for event in raw_events {
        event.verify()?;
        if mailbox_owners.contains_key(&event.group_id) {
            direct_events
                .entry((event.author_public_key.clone(), event.author_sequence))
                .or_default()
                .push(event);
        } else {
            group_events
                .entry((
                    event.group_id.clone(),
                    event.author_public_key.clone(),
                    event.author_sequence,
                ))
                .or_default()
                .push(event);
        }
    }

    let mut events = Vec::new();
    for mut candidates in group_events.into_values() {
        candidates.sort_by(|left, right| {
            left.created_at_millis
                .cmp(&right.created_at_millis)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        events.push(candidates.remove(0));
    }
    let mut direct_event_ids = BTreeSet::new();
    for ((author, _), candidates) in direct_events {
        let mut receiver = candidates
            .into_iter()
            .filter(|event| {
                mailbox_owners
                    .get(&event.group_id)
                    .is_some_and(|mailbox_owner| mailbox_owner != &author)
            })
            .collect::<Vec<_>>();
        if receiver.len() != 1 {
            bail!("verified direct-message envelope selection changed during import planning");
        }
        let event = receiver.remove(0);
        direct_event_ids.insert(event.event_id.clone());
        events.push(event);
    }
    events.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });

    let mut invitations = parse_objects::<InviteRecord>(&objects, "invite")?;
    let mut invite_rotations = parse_objects::<InviteRotation>(&objects, "invite_rotation")?;
    let mut group_deletions = parse_objects::<GroupDeletion>(&objects, "deletion")?;
    let mut mls_geneses = parse_objects::<MlsGroupGenesis>(&objects, "mls_genesis")?;
    let mut mls_epochs = parse_objects::<MlsEpochRecord>(&objects, "mls_epoch")?;
    let mut mls_join_requests = parse_objects::<MlsJoinRequest>(&objects, "mls_join_request")?;
    let mut mls_removal_requests =
        parse_objects::<MlsRemovalRequest>(&objects, "mls_removal_request")?;
    invitations.sort_by(|left, right| left.locator.cmp(&right.locator));
    invite_rotations.sort_by(|left, right| left.group_id.cmp(&right.group_id));
    group_deletions.sort_by(|left, right| left.group_id.cmp(&right.group_id));
    mls_geneses.sort_by(|left, right| left.group_id.cmp(&right.group_id));
    mls_epochs.sort_by(|left, right| {
        left.bundle
            .group_id
            .cmp(&right.bundle.group_id)
            .then_with(|| left.bundle.epoch.cmp(&right.bundle.epoch))
    });
    mls_join_requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
    mls_removal_requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));

    Ok(SocialPlan {
        accounts,
        events,
        direct_event_ids,
        invitations,
        invite_rotations,
        group_deletions,
        mls_geneses,
        mls_epochs,
        mls_join_requests,
        mls_removal_requests,
    })
}

fn parse_objects<T>(
    objects: &BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    kind: &str,
) -> anyhow::Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    objects
        .get(kind)
        .into_iter()
        .flat_map(BTreeMap::values)
        .map(|payload| serde_json::from_slice(payload).map_err(anyhow::Error::from))
        .collect()
}

pub async fn execute_plan(
    plan: &ImportPlan,
    store: Arc<dyn ObjectStore>,
    database_url: &str,
    upload_concurrency: usize,
) -> anyhow::Result<ImportSummary> {
    if upload_concurrency == 0 || upload_concurrency > MAX_UPLOAD_CONCURRENCY {
        bail!("upload concurrency must be between 1 and {MAX_UPLOAD_CONCURRENCY}");
    }
    upload_plan_objects(plan, store, upload_concurrency).await?;
    write_database(plan, database_url).await?;
    let mut summary = plan.summary.clone();
    summary.status = "imported";
    Ok(summary)
}

pub async fn execute_database_plan(
    plan: &ImportPlan,
    database_url: &str,
) -> anyhow::Result<ImportSummary> {
    write_database(plan, database_url).await?;
    let mut summary = plan.summary.clone();
    summary.status = "imported";
    Ok(summary)
}

pub fn local_object_store(root: &Path) -> anyhow::Result<Arc<dyn ObjectStore>> {
    fs::create_dir_all(root).context("could not create local migration object store")?;
    Ok(Arc::new(LocalFileSystem::new_with_prefix(root).context(
        "could not initialize local migration object store",
    )?))
}

pub fn r2_object_store_from_env() -> anyhow::Result<Arc<dyn ObjectStore>> {
    let endpoint = required_secret_env("NOISE_R2_ENDPOINT")?;
    validate_r2_endpoint(&endpoint)?;
    let bucket = required_secret_env("NOISE_R2_BUCKET")?;
    if !(3..=63).contains(&bucket.len())
        || !bucket
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !bucket
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !bucket
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("NOISE_R2_BUCKET is invalid");
    }
    let access_key = required_secret_env("NOISE_R2_ACCESS_KEY_ID")?;
    let secret_key = required_secret_env("NOISE_R2_SECRET_ACCESS_KEY")?;
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_region("auto")
        .with_bucket_name(bucket)
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_virtual_hosted_style_request(false)
        .with_disable_bulk_delete(true)
        .build()
        .context("could not initialize private R2 object store")?;
    Ok(Arc::new(store))
}

pub async fn upload_plan_objects(
    plan: &ImportPlan,
    store: Arc<dyn ObjectStore>,
    concurrency: usize,
) -> anyhow::Result<()> {
    let completed = Arc::new(AtomicU64::new(0));
    let total = plan.objects.len() as u64;
    let mut uploads = stream::iter(plan.objects.iter().cloned())
        .map(|object| {
            let store = store.clone();
            let completed = completed.clone();
            async move {
                upload_object(store.as_ref(), &object).await?;
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if current == total || current % 100 == 0 {
                    eprintln!("verified {current} of {total} encrypted R2 objects");
                }
                Ok::<(), anyhow::Error>(())
            }
        })
        .buffer_unordered(concurrency);
    while let Some(result) = uploads.next().await {
        result?;
    }
    Ok(())
}

async fn upload_object(store: &dyn ObjectStore, object: &ObjectPlan) -> anyhow::Result<()> {
    let source = fs::read(&object.source_path).context("migration media source disappeared")?;
    let normalized = normalize_legacy_media_payload(&source)?;
    if normalized.object_id != object.object_id
        || normalized.storage_bytes.len() as u64 != object.storage_length
        || blake3::hash(&normalized.storage_bytes).to_hex().as_str() != object.storage_hash
    {
        bail!("migration media source changed after planning");
    }
    let location =
        ObjectPath::parse(&object.storage_key).context("canonical R2 object key is invalid")?;
    let result = store
        .put_opts(
            &location,
            normalized.storage_bytes.clone().into(),
            PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            },
        )
        .await;
    match result {
        Ok(_) | Err(ObjectStoreError::AlreadyExists { .. }) => {}
        Err(error) => return Err(error).context("could not upload canonical encrypted object"),
    }
    let stored = store
        .get(&location)
        .await
        .context("could not read back canonical encrypted object")?
        .bytes()
        .await
        .context("could not verify canonical encrypted object")?;
    if stored.len() as u64 != object.storage_length
        || blake3::hash(&stored).to_hex().as_str() != object.storage_hash
    {
        bail!("stored canonical encrypted object failed read-back verification");
    }
    Ok(())
}

async fn write_database(plan: &ImportPlan, database_url: &str) -> anyhow::Result<()> {
    let config =
        tokio_postgres::Config::from_str(database_url).context("NOISE_DATABASE_URL is invalid")?;
    let (mut client, connection) = config
        .connect(NoTls)
        .await
        .context("could not connect to canonical PostgreSQL")?;
    let connection_task = tokio::spawn(async move { connection.await });
    let transaction = client
        .transaction()
        .await
        .context("could not begin media import transaction")?;
    let schema_version: i32 = transaction
        .query_one(
            "SELECT version FROM noise.schema_migrations ORDER BY version DESC LIMIT 1",
            &[],
        )
        .await
        .context("canonical schema is unavailable")?
        .get(0);
    if !(2..=3).contains(&schema_version) {
        bail!("canonical schema version {schema_version} is unsupported");
    }

    let mut provider_ids = BTreeMap::new();
    for (name, origin) in &plan.providers {
        let row = transaction
            .query_opt(
                "INSERT INTO noise.legacy_media_providers (
                    provider_name, compatibility_origin
                 ) VALUES ($1, $2)
                 ON CONFLICT (provider_name) DO UPDATE
                 SET provider_name = EXCLUDED.provider_name
                 WHERE noise.legacy_media_providers.compatibility_origin
                     = EXCLUDED.compatibility_origin
                 RETURNING provider_id",
                &[name, origin],
            )
            .await
            .context("could not reconcile legacy media provider")?
            .ok_or_else(|| anyhow!("legacy media provider conflicts with existing data"))?;
        provider_ids.insert(name.as_str(), row.get::<_, i16>(0));
    }
    for object in &plan.objects {
        insert_object(&transaction, object).await?;
    }
    for alias in &plan.aliases {
        let provider_id = provider_ids
            .get(alias.provider.as_str())
            .copied()
            .context("legacy media provider is missing")?;
        insert_alias(&transaction, provider_id, alias).await?;
    }
    import_social_data(&transaction, &plan.social).await?;
    transaction
        .commit()
        .await
        .context("could not commit media import")?;
    drop(client);
    connection_task
        .await
        .context("PostgreSQL connection task failed")?
        .context("PostgreSQL connection failed")?;
    Ok(())
}

async fn import_social_data(
    transaction: &Transaction<'_>,
    plan: &SocialPlan,
) -> anyhow::Result<()> {
    let existing_events: i64 = transaction
        .query_one("SELECT count(*) FROM noise.events", &[])
        .await?
        .get(0);
    if existing_events != 0 {
        bail!("social migration requires an empty canonical event store");
    }

    // The relay can retain signed group/MLS tombstones after an account vault
    // has expired or been deleted. Preserve every identity referenced by the
    // verified social graph so those records remain attributable even when
    // there is no vault left to migrate for that identity.
    let mut referenced_identities = BTreeSet::<String>::new();
    referenced_identities.extend(
        plan.accounts
            .iter()
            .map(|account| account.identity_public_key.clone()),
    );
    referenced_identities.extend(
        plan.group_deletions
            .iter()
            .map(|deletion| deletion.owner_public_key.clone()),
    );
    for genesis in &plan.mls_geneses {
        referenced_identities.insert(genesis.owner_public_key.clone());
        referenced_identities.extend(genesis.member_accounts.iter().cloned());
    }
    for epoch in &plan.mls_epochs {
        referenced_identities.insert(epoch.owner_public_key.clone());
        referenced_identities.extend(epoch.member_accounts.iter().cloned());
    }
    referenced_identities.extend(
        plan.mls_join_requests
            .iter()
            .map(|request| request.account_public_key.clone()),
    );
    for request in &plan.mls_removal_requests {
        referenced_identities.insert(request.requester_public_key.clone());
        referenced_identities.insert(request.target_public_key.clone());
    }
    referenced_identities.extend(
        plan.events
            .iter()
            .map(|event| event.author_public_key.clone()),
    );

    let mut account_ids = BTreeMap::<String, i64>::new();
    for public_key in referenced_identities {
        let identity = decode_base64_32(&public_key)?;
        let row = transaction
            .query_one(
                "INSERT INTO noise.accounts (identity_public_key)
                 VALUES ($1)
                 ON CONFLICT (identity_public_key) DO UPDATE
                 SET identity_public_key = EXCLUDED.identity_public_key
                 RETURNING account_id",
                &[&identity.as_slice()],
            )
            .await?;
        account_ids.insert(public_key, row.get(0));
    }

    for account in &plan.accounts {
        let identity = decode_base64_32(&account.identity_public_key)?;
        let row = transaction
            .query_one(
                "INSERT INTO noise.accounts (
                    identity_public_key, status, deleted_at
                 ) VALUES (
                    $1,
                    CASE WHEN $2 THEN 'deleted' ELSE 'active' END,
                    CASE WHEN $2 THEN clock_timestamp() ELSE NULL END
                 )
                 ON CONFLICT (identity_public_key) DO UPDATE
                 SET status = CASE
                         WHEN noise.accounts.status = 'blocked' THEN 'blocked'
                         WHEN EXCLUDED.status = 'deleted' THEN 'deleted'
                         ELSE 'active'
                     END,
                     deleted_at = CASE
                         WHEN EXCLUDED.status = 'deleted'
                             THEN COALESCE(noise.accounts.deleted_at, clock_timestamp())
                         ELSE NULL
                     END
                 RETURNING account_id",
                &[&identity.as_slice(), &account.deleted],
            )
            .await?;
        account_ids.insert(account.identity_public_key.clone(), row.get(0));
    }

    for account in &plan.accounts {
        let account_id = *account_ids
            .get(&account.identity_public_key)
            .context("selected account identity is missing")?;
        let locator = decode_hex_32(&account.locator)?;
        let signature = decode_base64_64(&account.signature_base64)?;
        let nonce = if account.deleted {
            None
        } else {
            Some(decode_base64_24(&account.nonce_base64)?.to_vec())
        };
        let ciphertext = if account.deleted {
            None
        } else {
            Some(decode_canonical_base64(&account.ciphertext_base64)?)
        };
        let revision = account.revision.to_string();
        let wire = serde_json::to_vec(account)?;
        transaction
            .execute(
                "INSERT INTO noise.account_vault_locators (locator, account_id)
                 VALUES ($1, $2)
                 ON CONFLICT (locator) DO UPDATE
                 SET account_id = EXCLUDED.account_id
                 WHERE noise.account_vault_locators.account_id = EXCLUDED.account_id",
                &[&locator.as_slice(), &account_id],
            )
            .await?;
        transaction
            .execute(
                "INSERT INTO noise.account_vault_versions (
                    locator, revision, nonce, ciphertext, deleted, signature,
                    signed_wire_record
                 ) VALUES ($1, $2::text::numeric, $3, $4, $5, $6, $7)
                 ON CONFLICT (locator, revision) DO NOTHING",
                &[
                    &locator.as_slice(),
                    &revision,
                    &nonce,
                    &ciphertext,
                    &account.deleted,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
        transaction
            .execute(
                "INSERT INTO noise.account_vault_heads (locator, revision)
                 VALUES ($1, $2::text::numeric)
                 ON CONFLICT (locator) DO UPDATE
                 SET revision = GREATEST(
                     noise.account_vault_heads.revision,
                     EXCLUDED.revision
                 ),
                 updated_at = clock_timestamp()",
                &[&locator.as_slice(), &revision],
            )
            .await?;
    }

    let mut group_founders = BTreeMap::<String, String>::new();
    for genesis in &plan.mls_geneses {
        group_founders.insert(genesis.group_id.clone(), genesis.owner_public_key.clone());
    }
    for deletion in &plan.group_deletions {
        group_founders
            .entry(deletion.group_id.clone())
            .or_insert_with(|| deletion.owner_public_key.clone());
    }
    for event in plan
        .events
        .iter()
        .filter(|event| !plan.direct_event_ids.contains(&event.event_id))
    {
        group_founders
            .entry(event.group_id.clone())
            .or_insert_with(|| event.author_public_key.clone());
    }

    let deleted_groups = plan
        .group_deletions
        .iter()
        .map(|deletion| deletion.group_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut group_ids = BTreeMap::<String, i64>::new();
    for (group_id, founder_public_key) in &group_founders {
        let protocol_group_id = decode_hex_32(group_id)?;
        let founder_account_id = account_ids.get(founder_public_key).copied();
        let deleted = deleted_groups.contains(group_id.as_str());
        let row = transaction
            .query_one(
                "INSERT INTO noise.groups (
                    protocol_group_id, founder_account_id, lifecycle_state, deleted_at
                 ) VALUES (
                    $1, $2,
                    CASE WHEN $3 THEN 'deleted' ELSE 'active' END,
                    CASE WHEN $3 THEN clock_timestamp() ELSE NULL END
                 )
                 ON CONFLICT (protocol_group_id) DO UPDATE
                 SET founder_account_id = COALESCE(
                         noise.groups.founder_account_id,
                         EXCLUDED.founder_account_id
                     ),
                     lifecycle_state = EXCLUDED.lifecycle_state,
                     deleted_at = EXCLUDED.deleted_at
                 RETURNING group_pk",
                &[&protocol_group_id.as_slice(), &founder_account_id, &deleted],
            )
            .await?;
        group_ids.insert(group_id.clone(), row.get(0));
    }

    for deletion in &plan.group_deletions {
        let group_pk = *group_ids
            .get(&deletion.group_id)
            .context("deleted group is missing")?;
        let owner_account_id = *account_ids
            .get(&deletion.owner_public_key)
            .context("group deletion owner is missing")?;
        let signature = decode_base64_64(&deletion.signature_base64)?;
        let deleted_at = deletion.deleted_at_millis.to_string();
        let wire = serde_json::to_vec(deletion)?;
        transaction
            .execute(
                "INSERT INTO noise.group_deletions (
                    group_pk, owner_account_id, deleted_at_millis,
                    signature, signed_wire_record
                 ) VALUES ($1, $2, $3::text::numeric, $4, $5)
                 ON CONFLICT (group_pk) DO NOTHING",
                &[
                    &group_pk,
                    &owner_account_id,
                    &deleted_at,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
    }

    for genesis in &plan.mls_geneses {
        let group_pk = *group_ids
            .get(&genesis.group_id)
            .context("MLS genesis group is missing")?;
        let founder_account_id = *account_ids
            .get(&genesis.owner_public_key)
            .context("MLS founder account is missing")?;
        let record_id = decode_hex_32(&genesis.record_id)?;
        let authority_nonce = decode_base64_32(&genesis.authority_nonce_base64)?;
        let signature = decode_base64_64(&genesis.signature_base64)?;
        let created_at = genesis.created_at_millis.to_string();
        let wire = serde_json::to_vec(genesis)?;
        transaction
            .execute(
                "INSERT INTO noise.mls_geneses (
                    record_id, group_pk, founder_account_id, authority_nonce,
                    created_at_millis, signature, signed_wire_record
                 ) VALUES (
                    $1, $2, $3, $4, $5::text::numeric, $6, $7
                 )
                 ON CONFLICT (record_id) DO NOTHING",
                &[
                    &record_id.as_slice(),
                    &group_pk,
                    &founder_account_id,
                    &authority_nonce.as_slice(),
                    &created_at,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
    }

    for epoch in &plan.mls_epochs {
        let group_pk = *group_ids
            .get(&epoch.bundle.group_id)
            .context("MLS epoch group is missing")?;
        let author_account_id = *account_ids
            .get(&epoch.owner_public_key)
            .context("MLS epoch author account is missing")?;
        let record_id = decode_hex_32(&epoch.record_id)?;
        let previous_record_id = decode_hex_32(&epoch.previous_record_id)?;
        let signature = decode_base64_64(&epoch.signature_base64)?;
        let parent_epoch = epoch.bundle.parent_epoch.to_string();
        let epoch_number = epoch.bundle.epoch.to_string();
        let created_at = epoch.created_at_millis.to_string();
        let wire = serde_json::to_vec(epoch)?;
        transaction
            .execute(
                "INSERT INTO noise.mls_epochs (
                    record_id, group_pk, previous_record_id, parent_epoch,
                    epoch, author_account_id, created_at_millis, signature,
                    signed_wire_record
                 ) VALUES (
                    $1, $2, $3, $4::text::numeric, $5::text::numeric,
                    $6, $7::text::numeric, $8, $9
                 )
                 ON CONFLICT (record_id) DO NOTHING",
                &[
                    &record_id.as_slice(),
                    &group_pk,
                    &previous_record_id.as_slice(),
                    &parent_epoch,
                    &epoch_number,
                    &author_account_id,
                    &created_at,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
        for member in &epoch.member_accounts {
            let member_account_id = *account_ids
                .get(member)
                .context("MLS epoch member account is missing")?;
            transaction
                .execute(
                    "INSERT INTO noise.mls_epoch_members (
                        epoch_record_id, account_id
                     ) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING",
                    &[&record_id.as_slice(), &member_account_id],
                )
                .await?;
        }
    }

    for genesis in &plan.mls_geneses {
        let group_pk = *group_ids
            .get(&genesis.group_id)
            .context("MLS membership group is missing")?;
        let (source_kind, source_record_id, members): (&str, Vec<u8>, Vec<String>) =
            if let Some(epoch) = plan
                .mls_epochs
                .iter()
                .filter(|epoch| epoch.bundle.group_id == genesis.group_id)
                .max_by_key(|epoch| epoch.bundle.epoch)
            {
                (
                    "mls_epoch",
                    decode_hex_32(&epoch.record_id)?,
                    epoch.member_accounts.clone(),
                )
            } else {
                (
                    "mls_genesis",
                    decode_hex_32(&genesis.record_id)?,
                    genesis.member_accounts.clone(),
                )
            };
        for member in members {
            let account_id = *account_ids
                .get(&member)
                .context("current MLS member account is missing")?;
            let role = if member == genesis.owner_public_key {
                "founder"
            } else {
                "member"
            };
            transaction
                .execute(
                    "INSERT INTO noise.group_memberships (
                        group_pk, account_id, role, source_kind,
                        source_record_id, active_from_cursor
                     ) VALUES ($1, $2, $3, $4, $5, 0)
                     ON CONFLICT (group_pk, account_id)
                         WHERE active_until_cursor IS NULL
                     DO UPDATE SET
                         role = EXCLUDED.role,
                         source_kind = EXCLUDED.source_kind,
                         source_record_id = EXCLUDED.source_record_id",
                    &[
                        &group_pk,
                        &account_id,
                        &role,
                        &source_kind,
                        &source_record_id.as_slice(),
                    ],
                )
                .await?;
        }
    }

    for request in &plan.mls_join_requests {
        let Some(group_pk) = group_ids.get(&request.group_id).copied() else {
            continue;
        };
        let account_id = *account_ids
            .get(&request.account_public_key)
            .context("MLS join account is missing")?;
        let request_id = decode_hex_32(&request.request_id)?;
        let signature = decode_base64_64(&request.signature_base64)?;
        let created_at = request.created_at_millis.to_string();
        let wire = serde_json::to_vec(request)?;
        transaction
            .execute(
                "INSERT INTO noise.mls_join_requests (
                    request_id, group_pk, account_id, created_at_millis,
                    signature, signed_wire_record
                 ) VALUES ($1, $2, $3, $4::text::numeric, $5, $6)
                 ON CONFLICT (request_id) DO NOTHING",
                &[
                    &request_id.as_slice(),
                    &group_pk,
                    &account_id,
                    &created_at,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
    }

    for request in &plan.mls_removal_requests {
        let Some(group_pk) = group_ids.get(&request.group_id).copied() else {
            continue;
        };
        let requester_account_id = *account_ids
            .get(&request.requester_public_key)
            .context("MLS removal requester is missing")?;
        let target_account_id = *account_ids
            .get(&request.target_public_key)
            .context("MLS removal target is missing")?;
        let request_id = decode_hex_32(&request.request_id)?;
        let signature = decode_base64_64(&request.signature_base64)?;
        let created_at = request.created_at_millis.to_string();
        let reason = match request.reason {
            MlsRemovalReason::SelfLeft => "self_left",
            MlsRemovalReason::Banned => "banned",
        };
        let wire = serde_json::to_vec(request)?;
        transaction
            .execute(
                "INSERT INTO noise.mls_removal_requests (
                    request_id, group_pk, requester_account_id,
                    target_account_id, reason, delete_messages,
                    created_at_millis, signature, signed_wire_record
                 ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7::text::numeric, $8, $9
                 )
                 ON CONFLICT (request_id) DO NOTHING",
                &[
                    &request_id.as_slice(),
                    &group_pk,
                    &requester_account_id,
                    &target_account_id,
                    &reason,
                    &request.delete_messages,
                    &created_at,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
    }

    import_events(transaction, plan, &account_ids, &group_ids).await?;
    import_invitations(transaction, plan, &group_ids).await?;
    Ok(())
}

async fn import_events(
    transaction: &Transaction<'_>,
    plan: &SocialPlan,
    account_ids: &BTreeMap<String, i64>,
    group_ids: &BTreeMap<String, i64>,
) -> anyhow::Result<()> {
    let mut stream_ids = BTreeMap::<(String, Option<String>), i64>::new();
    let mut direct_thread_ids = BTreeMap::<String, i64>::new();
    let mut latest_by_stream = BTreeMap::<i64, i64>::new();
    let mut cursor = transaction
        .query_one(
            "SELECT last_cursor FROM noise.cursor_clock WHERE singleton FOR UPDATE",
            &[],
        )
        .await?
        .get::<_, i64>(0);

    let mailbox_owners = account_ids
        .keys()
        .map(|identity| Ok((direct_mailbox_id(identity)?, identity.clone())))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;

    for event in &plan.events {
        let author_account_id = *account_ids
            .get(&event.author_public_key)
            .context("event author account is missing")?;
        let event_id = decode_hex_32(&event.event_id)?;
        let nonce = decode_base64_24(&event.nonce_base64)?;
        let ciphertext = decode_canonical_base64(&event.ciphertext_base64)?;
        let signature = decode_base64_64(&event.signature_base64)?;
        let author_sequence = event.author_sequence.to_string();
        let created_at = event.created_at_millis.to_string();
        let epoch = event.epoch.map(|value| value.to_string());
        let wire = serde_json::to_vec(event)?;
        cursor = cursor.checked_add(1).context("canonical cursor overflow")?;

        let (scope_kind, protocol_scope_id, group_pk, direct_thread_pk, stream_pk, stream_locator) =
            if plan.direct_event_ids.contains(&event.event_id) {
                let recipient = mailbox_owners
                    .get(&event.group_id)
                    .context("canonical direct recipient is missing")?;
                let recipient_account_id = *account_ids
                    .get(recipient)
                    .context("canonical direct recipient account is missing")?;
                let scope_id = direct_scope_id(&event.author_public_key, recipient)?;
                let scope_bytes = decode_hex_32(&scope_id)?;
                let direct_thread_pk = if let Some(thread) = direct_thread_ids.get(&scope_id) {
                    *thread
                } else {
                    let (low, high) = if author_account_id < recipient_account_id {
                        (author_account_id, recipient_account_id)
                    } else {
                        (recipient_account_id, author_account_id)
                    };
                    let row = transaction
                        .query_one(
                            "INSERT INTO noise.direct_threads (
                            protocol_scope_id, account_low_id, account_high_id
                         ) VALUES ($1, $2, $3)
                         ON CONFLICT (account_low_id, account_high_id) DO UPDATE
                         SET protocol_scope_id = EXCLUDED.protocol_scope_id
                         WHERE noise.direct_threads.protocol_scope_id
                             = EXCLUDED.protocol_scope_id
                         RETURNING direct_thread_pk",
                            &[&scope_bytes.as_slice(), &low, &high],
                        )
                        .await?;
                    let thread = row.get::<_, i64>(0);
                    direct_thread_ids.insert(scope_id.clone(), thread);
                    thread
                };
                let stream_key = (scope_id.clone(), None);
                let stream_pk = if let Some(stream) = stream_ids.get(&stream_key) {
                    *stream
                } else {
                    let row = transaction
                        .query_one(
                            "INSERT INTO noise.streams (
                            stream_kind, direct_thread_pk, protocol_stream_locator
                         ) VALUES ('direct', $1, $2)
                         ON CONFLICT (direct_thread_pk)
                             WHERE stream_kind = 'direct'
                         DO UPDATE SET
                             protocol_stream_locator = EXCLUDED.protocol_stream_locator
                         RETURNING stream_pk",
                            &[&direct_thread_pk, &scope_bytes.as_slice()],
                        )
                        .await?;
                    let stream = row.get::<_, i64>(0);
                    stream_ids.insert(stream_key, stream);
                    stream
                };
                (
                    "direct",
                    scope_bytes,
                    None,
                    Some(direct_thread_pk),
                    stream_pk,
                    None,
                )
            } else {
                let group_pk = *group_ids
                    .get(&event.group_id)
                    .context("group event scope is missing")?;
                let protocol_scope_id = decode_hex_32(&event.group_id)?;
                let stream_locator = event
                    .stream_locator
                    .as_deref()
                    .map(decode_hex_32)
                    .transpose()?;
                let stream_key = (event.group_id.clone(), event.stream_locator.clone());
                let stream_pk = if let Some(stream) = stream_ids.get(&stream_key) {
                    *stream
                } else {
                    let stream_kind = if stream_locator.is_some() {
                        "topic"
                    } else {
                        "group"
                    };
                    let row = transaction
                        .query_one(
                            "INSERT INTO noise.streams (
                            stream_kind, group_pk, protocol_stream_locator
                         ) VALUES ($1, $2, $3)
                         ON CONFLICT DO NOTHING
                         RETURNING stream_pk",
                            &[
                                &stream_kind,
                                &group_pk,
                                &stream_locator.as_ref().map(Vec::as_slice),
                            ],
                        )
                        .await?;
                    let stream = row.get::<_, i64>(0);
                    stream_ids.insert(stream_key, stream);
                    stream
                };
                (
                    "group",
                    protocol_scope_id,
                    Some(group_pk),
                    None,
                    stream_pk,
                    stream_locator,
                )
            };

        transaction
            .execute(
                "INSERT INTO noise.events (
                    event_id, canonical_cursor, scope_kind, protocol_scope_id,
                    group_pk, direct_thread_pk, stream_pk, author_account_id,
                    author_sequence, created_at_millis, encryption_version,
                    epoch, protocol_stream_locator, nonce, ciphertext,
                    signature, signed_wire_record
                 ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8,
                    $9::text::numeric, $10::text::numeric, $11,
                    $12::text::numeric, $13, $14, $15, $16, $17
                 )",
                &[
                    &event_id.as_slice(),
                    &cursor,
                    &scope_kind,
                    &protocol_scope_id.as_slice(),
                    &group_pk,
                    &direct_thread_pk,
                    &stream_pk,
                    &author_account_id,
                    &author_sequence,
                    &created_at,
                    &(event.encryption_version as i32),
                    &epoch,
                    &stream_locator.as_ref().map(Vec::as_slice),
                    &nonce.as_slice(),
                    &ciphertext,
                    &signature.as_slice(),
                    &wire,
                ],
            )
            .await?;
        latest_by_stream.insert(stream_pk, cursor);
    }

    for (stream_pk, latest_cursor) in latest_by_stream {
        transaction
            .execute(
                "UPDATE noise.streams
                 SET latest_cursor = $2
                 WHERE stream_pk = $1",
                &[&stream_pk, &latest_cursor],
            )
            .await?;
    }
    transaction
        .execute(
            "UPDATE noise.cursor_clock SET last_cursor = $1 WHERE singleton",
            &[&cursor],
        )
        .await?;
    Ok(())
}

async fn import_invitations(
    transaction: &Transaction<'_>,
    plan: &SocialPlan,
    group_ids: &BTreeMap<String, i64>,
) -> anyhow::Result<()> {
    let mut invitation_by_group = BTreeMap::<String, Vec<u8>>::new();
    for invitation in &plan.invitations {
        let Some(group_id) = invitation.group_id.as_deref() else {
            continue;
        };
        let Some(group_pk) = group_ids.get(group_id).copied() else {
            continue;
        };
        let invitation_id = decode_hex_32(&invitation.locator)?;
        let wire = serde_json::to_vec(invitation)?;
        transaction
            .execute(
                "INSERT INTO noise.legacy_invitations (
                    invitation_id, lookup_hash, group_pk, generation,
                    signed_wire_record
                 ) VALUES ($1, $1, $2, 0, $3)
                 ON CONFLICT (invitation_id) DO NOTHING",
                &[&invitation_id.as_slice(), &group_pk, &wire],
            )
            .await?;
        invitation_by_group.insert(group_id.to_owned(), invitation_id);
    }

    for rotation in &plan.invite_rotations {
        let invitation_id = if let Some(new_invite) = rotation.new_invite.as_ref() {
            let group_pk = *group_ids
                .get(&rotation.group_id)
                .context("invite rotation group is missing")?;
            let invitation_id = decode_hex_32(&new_invite.locator)?;
            let wire = serde_json::to_vec(new_invite)?;
            transaction
                .execute(
                    "INSERT INTO noise.legacy_invitations (
                        invitation_id, lookup_hash, group_pk, generation,
                        signed_wire_record
                     ) VALUES ($1, $1, $2, $3::text::numeric, $4)
                     ON CONFLICT (invitation_id) DO NOTHING",
                    &[
                        &invitation_id.as_slice(),
                        &group_pk,
                        &rotation.owner_sequence.to_string(),
                        &wire,
                    ],
                )
                .await?;
            invitation_by_group.insert(rotation.group_id.clone(), invitation_id.clone());
            invitation_id
        } else {
            invitation_by_group
                .get(&rotation.group_id)
                .cloned()
                .context("disabled invite rotation has no prior invitation")?
        };
        let rotation_id = decode_hex_32(&rotation.group_id)?;
        let generation = rotation.owner_sequence.to_string();
        let wire = serde_json::to_vec(rotation)?;
        transaction
            .execute(
                "INSERT INTO noise.legacy_invite_rotations (
                    rotation_id, invitation_id, generation, signed_wire_record
                 ) VALUES ($1, $2, $3::text::numeric, $4)
                 ON CONFLICT (rotation_id) DO NOTHING",
                &[
                    &rotation_id.as_slice(),
                    &invitation_id.as_slice(),
                    &generation,
                    &wire,
                ],
            )
            .await?;
    }
    Ok(())
}

async fn insert_object(transaction: &Transaction<'_>, object: &ObjectPlan) -> anyhow::Result<()> {
    let object_id = decode_hex_32(&object.object_id)?;
    let payload_hash = decode_hex_32(&object.storage_hash)?;
    let storage_length = i64::try_from(object.storage_length)?;
    let inserted = transaction
        .query_opt(
            "INSERT INTO noise.legacy_media_objects (
                media_object_id, storage_key, storage_byte_length,
                storage_payload_hash, state
             ) VALUES ($1, $2, $3, $4, 'available')
             ON CONFLICT (media_object_id) DO UPDATE
             SET imported_at = noise.legacy_media_objects.imported_at
             WHERE noise.legacy_media_objects.storage_key = EXCLUDED.storage_key
               AND noise.legacy_media_objects.storage_byte_length
                   = EXCLUDED.storage_byte_length
               AND noise.legacy_media_objects.storage_payload_hash
                   = EXCLUDED.storage_payload_hash
               AND noise.legacy_media_objects.state = 'available'
             RETURNING 1",
            &[
                &object_id,
                &object.storage_key,
                &storage_length,
                &payload_hash,
            ],
        )
        .await
        .context("could not import canonical encrypted media object")?;
    if inserted.is_none() {
        bail!("canonical encrypted media object conflicts with existing data");
    }
    Ok(())
}

async fn insert_alias(
    transaction: &Transaction<'_>,
    provider_id: i16,
    alias: &AliasPlan,
) -> anyhow::Result<()> {
    let inserted = transaction
        .query_opt(
            "INSERT INTO noise.legacy_media_shards (
                provider_id, shard_id, state, payload_hash, ciphertext_length,
                canonical_object_id, payload_encoding,
                deletion_capability_hash, tombstoned_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL)
             ON CONFLICT (provider_id, shard_id) DO UPDATE
             SET accepted_at = noise.legacy_media_shards.accepted_at
             WHERE noise.legacy_media_shards.state = EXCLUDED.state
               AND noise.legacy_media_shards.payload_hash
                   IS NOT DISTINCT FROM EXCLUDED.payload_hash
               AND noise.legacy_media_shards.ciphertext_length
                   IS NOT DISTINCT FROM EXCLUDED.ciphertext_length
               AND noise.legacy_media_shards.canonical_object_id
                   IS NOT DISTINCT FROM EXCLUDED.canonical_object_id
               AND noise.legacy_media_shards.payload_encoding
                   IS NOT DISTINCT FROM EXCLUDED.payload_encoding
               AND noise.legacy_media_shards.deletion_capability_hash
                   IS NOT DISTINCT FROM EXCLUDED.deletion_capability_hash
             RETURNING 1",
            &[
                &provider_id,
                &alias.shard_id,
                &alias.state,
                &alias.payload_hash,
                &alias.ciphertext_length,
                &alias.canonical_object_id,
                &alias.payload_encoding,
                &alias.deletion_capability_hash,
            ],
        )
        .await
        .context("could not import legacy media alias")?;
    if inserted.is_none() {
        bail!("legacy media alias conflicts with existing data");
    }
    Ok(())
}

fn open_snapshot(path: &Path) -> anyhow::Result<Connection> {
    let uri = immutable_sqlite_uri(path)?;
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn read_id_set(connection: &Connection, table: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut statement =
        connection.prepare(&format!("SELECT shard_id FROM {table} ORDER BY shard_id"))?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(values)
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

fn canonical_compatibility_origin(value: &str) -> anyhow::Result<String> {
    let parsed = Url::parse(value).context("compatibility origin is invalid")?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("compatibility origin must be an HTTPS origin without a path");
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn validate_r2_endpoint(value: &str) -> anyhow::Result<()> {
    let parsed = Url::parse(value).context("NOISE_R2_ENDPOINT is invalid")?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https"
        || !host.ends_with(".r2.cloudflarestorage.com")
        || host == "r2.cloudflarestorage.com"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("NOISE_R2_ENDPOINT must be an account-specific Cloudflare R2 HTTPS endpoint");
    }
    Ok(())
}

fn required_secret_env(name: &str) -> anyhow::Result<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is required"))
}

fn canonical_storage_key(object_id: &str) -> anyhow::Result<String> {
    decode_hex_32(object_id)?;
    Ok(format!(
        "legacy/objects/{}/{}.nsb2",
        &object_id[..2],
        object_id
    ))
}

fn decode_hex_32(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a canonical 32-byte hexadecimal identifier");
    }
    let mut output = Vec::with_capacity(32);
    for index in (0..64).step_by(2) {
        output.push(
            u8::from_str_radix(&value[index..index + 2], 16)
                .context("hexadecimal identifier is invalid")?,
        );
    }
    Ok(output)
}

fn decode_canonical_base64(value: &str) -> anyhow::Result<Vec<u8>> {
    let decoded = STANDARD_NO_PAD
        .decode(value)
        .context("base64 value is invalid")?;
    if STANDARD_NO_PAD.encode(&decoded) != value {
        bail!("base64 value is not canonical");
    }
    Ok(decoded)
}

fn decode_base64_24(value: &str) -> anyhow::Result<[u8; 24]> {
    decode_base64_array(value)
}

fn decode_base64_32(value: &str) -> anyhow::Result<[u8; 32]> {
    decode_base64_array(value)
}

fn decode_base64_64(value: &str) -> anyhow::Result<[u8; 64]> {
    decode_base64_array(value)
}

fn decode_base64_array<const N: usize>(value: &str) -> anyhow::Result<[u8; N]> {
    let decoded = decode_canonical_base64(value)?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("base64 value has the wrong length"))
}
