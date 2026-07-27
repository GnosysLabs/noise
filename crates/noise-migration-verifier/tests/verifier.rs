use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use noise_core::{
    AccountVault, DirectMessagePolicy, GroupMembership, Identity, Profile, SignedEvent,
};
use noise_migration_verifier::{SourceInput, verify};
use rusqlite::{Connection, params};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn valid_snapshots_merge_without_exposing_duplicate_direct_copies() {
    let fixture = Fixture::new();
    let report = verify(fixture.inputs(), "primary").unwrap();

    assert_eq!(report.status, "pass");
    assert!(report.blockers.is_empty());
    assert_eq!(report.accounts.unique_locators, 2);
    assert_eq!(report.accounts.selected_deleted, 2);
    assert_eq!(report.accounts.differing_source_revisions, 1);
    assert_eq!(report.events.raw_unique_events, 3);
    assert_eq!(report.events.group_events, 1);
    assert_eq!(report.events.canonical_group_events, 1);
    assert_eq!(report.events.direct_mailbox_copies, 2);
    assert_eq!(report.events.canonical_direct_events, 1);
    assert_eq!(report.events.collapsed_direct_copies, 1);
    assert_eq!(report.events.canonical_event_count, 2);
    assert_eq!(report.media.provider_scoped_live_shards, 2);
    assert_eq!(report.media.unique_payloads, 1);
    assert_eq!(report.media.duplicate_payload_references, 1);
}

#[test]
fn conflicting_wire_payload_for_one_immutable_id_blocks_migration() {
    let fixture = Fixture::new();
    let database = Connection::open(&fixture.secondary_database).unwrap();
    database
        .execute(
            "UPDATE relay_objects
             SET payload = ?1
             WHERE kind = 'event' AND object_id = ?2",
            params![
                serde_json::to_string_pretty(&fixture.group_event).unwrap(),
                fixture.group_event.event_id,
            ],
        )
        .unwrap();
    drop(database);

    let report = verify(fixture.inputs(), "primary").unwrap();
    assert_eq!(report.status, "blocked");
    assert_blocker(&report, "immutable_event_payload_conflict", 1);
}

#[test]
fn conflicting_author_sequence_blocks_migration() {
    let fixture = Fixture::new();
    let conflicting = SignedEvent::chat(&fixture.alice, &fixture.group, "equivocation", 1).unwrap();
    let database = Connection::open(&fixture.secondary_database).unwrap();
    insert_object(&database, "event", &conflicting.event_id, &conflicting);
    drop(database);

    let report = verify(fixture.inputs(), "primary").unwrap();
    assert_eq!(report.status, "blocked");
    assert_blocker(&report, "cross_source_group_sequence_conflict", 1);
}

#[test]
fn shared_legacy_author_sequence_duplicates_are_reduced() {
    let fixture = Fixture::new();
    let duplicate =
        SignedEvent::chat(&fixture.alice, &fixture.group, "legacy duplicate", 1).unwrap();
    for path in [&fixture.primary_database, &fixture.secondary_database] {
        let database = Connection::open(path).unwrap();
        insert_object(&database, "event", &duplicate.event_id, &duplicate);
    }

    let report = verify(fixture.inputs(), "primary").unwrap();
    assert_eq!(report.status, "pass");
    assert_eq!(report.events.group_events, 2);
    assert_eq!(report.events.canonical_group_events, 1);
    assert_eq!(report.events.shared_legacy_group_sequence_duplicates, 1);
    assert_eq!(report.events.canonical_event_count, 2);
    assert!(report.warnings.iter().any(|warning| warning.code
        == "shared_legacy_group_sequence_duplicate"
        && warning.count == 1));
}

#[test]
fn filesystem_orphan_blocks_until_it_is_classified() {
    let fixture = Fixture::new();
    let orphan_id = "f".repeat(64);
    let orphan_directory = fixture.primary_media.join("shards").join("ff");
    fs::create_dir_all(&orphan_directory).unwrap();
    fs::write(orphan_directory.join(format!("{orphan_id}.bin")), b"orphan").unwrap();

    let report = verify(fixture.inputs(), "primary").unwrap();
    assert_eq!(report.status, "blocked");
    assert_blocker(&report, "unclassified_orphan_shard_file", 1);
}

fn assert_blocker(report: &noise_migration_verifier::VerificationReport, code: &str, count: u64) {
    assert!(
        report
            .blockers
            .iter()
            .any(|blocker| blocker.code == code && blocker.count == count),
        "missing blocker {code}"
    );
}

struct Fixture {
    root: PathBuf,
    primary_database: PathBuf,
    secondary_database: PathBuf,
    primary_media: PathBuf,
    secondary_media: PathBuf,
    alice: Identity,
    group: GroupMembership,
    group_event: SignedEvent,
}

impl Fixture {
    fn new() -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "noise-migration-verifier-{}-{sequence}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let primary_database = root.join("primary.db");
        let secondary_database = root.join("secondary.db");
        let primary_media = root.join("primary-media");
        let secondary_media = root.join("secondary-media");

        let alice =
            Identity::from_secret_base64("ERERERERERERERERERERERERERERERERERERERERERE").unwrap();
        let bob =
            Identity::from_secret_base64("IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI").unwrap();
        let alice_v1 = AccountVault::tombstone(&alice, "a".repeat(64), 1).unwrap();
        let alice_v2 = AccountVault::tombstone(&alice, "a".repeat(64), 2).unwrap();
        let bob_v1 = AccountVault::tombstone(&bob, "b".repeat(64), 1).unwrap();
        let group = GroupMembership::create_owned("fixture", alice.public_key_base64());
        let group_event = SignedEvent::chat(&alice, &group, "group event", 1).unwrap();
        let bob_public_key = bob.public_key_base64();
        let alice_public_key = alice.public_key_base64();
        let profile = Profile {
            username: "alice".into(),
            bio: String::new(),
            avatar: None,
            album: None,
            accepts_direct_messages: true,
            direct_message_policy: DirectMessagePolicy::Everyone,
        };
        let receiver_mailbox = alice
            .direct_mailbox(&bob_public_key, &bob_public_key)
            .unwrap();
        let sender_mailbox = alice
            .direct_mailbox(&bob_public_key, &alice_public_key)
            .unwrap();
        let receiver_copy = SignedEvent::direct_message(
            &alice,
            &receiver_mailbox,
            &bob_public_key,
            &profile,
            "direct event",
            None,
            None,
            2,
        )
        .unwrap();
        let sender_copy = SignedEvent::direct_message(
            &alice,
            &sender_mailbox,
            &bob_public_key,
            &profile,
            "direct event",
            None,
            None,
            2,
        )
        .unwrap();
        let shard_payload = b"opaque encrypted fixture bytes";
        let shard_id = "c".repeat(64);
        let payload_hash = blake3::hash(shard_payload).to_hex().to_string();

        create_snapshot(
            &primary_database,
            &primary_media,
            [&alice_v1, &bob_v1],
            [&group_event, &receiver_copy, &sender_copy],
            &shard_id,
            &payload_hash,
            shard_payload,
        );
        create_snapshot(
            &secondary_database,
            &secondary_media,
            [&alice_v2, &bob_v1],
            [&group_event, &receiver_copy, &sender_copy],
            &shard_id,
            &payload_hash,
            shard_payload,
        );

        Self {
            root,
            primary_database,
            secondary_database,
            primary_media,
            secondary_media,
            alice,
            group,
            group_event,
        }
    }

    fn inputs(&self) -> Vec<SourceInput> {
        vec![
            SourceInput {
                label: "primary".into(),
                database_path: self.primary_database.clone(),
                media_root: self.primary_media.clone(),
            },
            SourceInput {
                label: "secondary".into(),
                database_path: self.secondary_database.clone(),
                media_root: self.secondary_media.clone(),
            },
        ]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn create_snapshot<'a>(
    database_path: &Path,
    media_root: &Path,
    accounts: impl IntoIterator<Item = &'a AccountVault>,
    events: impl IntoIterator<Item = &'a SignedEvent>,
    shard_id: &str,
    payload_hash: &str,
    shard_payload: &[u8],
) {
    let database = Connection::open(database_path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE relay_objects (
                kind TEXT NOT NULL,
                object_id TEXT NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (kind, object_id)
             );
             CREATE TABLE relay_directory (
                relay_id TEXT PRIMARY KEY,
                descriptor TEXT NOT NULL
             );
             CREATE TABLE relay_shards (
                shard_id TEXT PRIMARY KEY,
                payload_hash TEXT NOT NULL,
                delete_token_hash TEXT NOT NULL,
                byte_length INTEGER NOT NULL
             );
             CREATE TABLE relay_shard_delete_queue (
                shard_id TEXT PRIMARY KEY
             );
             CREATE TABLE relay_shard_tombstones (
                shard_id TEXT PRIMARY KEY
             );
             CREATE TABLE push_subscriptions (
                mailbox_id TEXT NOT NULL,
                public_key TEXT NOT NULL,
                installation_id TEXT NOT NULL,
                device_token TEXT NOT NULL,
                environment TEXT NOT NULL,
                topic TEXT NOT NULL,
                updated_at_millis INTEGER NOT NULL,
                PRIMARY KEY (mailbox_id, installation_id)
             );
             CREATE INDEX push_subscriptions_mailbox_idx
                 ON push_subscriptions (mailbox_id);
             CREATE TABLE push_deliveries (
                mailbox_id TEXT NOT NULL,
                installation_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                delivered_at_millis INTEGER NOT NULL,
                PRIMARY KEY (mailbox_id, installation_id, event_id)
             );",
        )
        .unwrap();
    for account in accounts {
        insert_object(&database, "account", &account.locator, account);
    }
    for event in events {
        insert_object(&database, "event", &event.event_id, event);
    }
    database
        .execute(
            "INSERT INTO relay_shards (
                shard_id, payload_hash, delete_token_hash, byte_length
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                shard_id,
                payload_hash,
                "d".repeat(64),
                shard_payload.len() as i64,
            ],
        )
        .unwrap();
    drop(database);

    let shard_directory = media_root.join("shards").join(&shard_id[..2]);
    fs::create_dir_all(&shard_directory).unwrap();
    fs::write(
        shard_directory.join(format!("{shard_id}.bin")),
        shard_payload,
    )
    .unwrap();
}

fn insert_object<T: serde::Serialize>(
    database: &Connection,
    kind: &str,
    object_id: &str,
    value: &T,
) {
    database
        .execute(
            "INSERT INTO relay_objects (kind, object_id, payload)
             VALUES (?1, ?2, ?3)",
            params![kind, object_id, serde_json::to_string(value).unwrap()],
        )
        .unwrap();
}
