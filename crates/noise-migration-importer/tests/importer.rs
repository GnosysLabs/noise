use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use noise_core::EncryptedBlob;
use noise_migration_importer::{ImportSource, build_plan, local_object_store, upload_plan_objects};
use noise_migration_verifier::SourceInput;
use rusqlite::{Connection, params};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn canonical_media_plan_deduplicates_encodings_and_resumes_uploads() {
    let fixture = Fixture::new();
    let plan = build_plan(fixture.sources(), "primary").unwrap();
    assert_eq!(plan.summary.status, "ready");
    assert_eq!(plan.summary.providers, 2);
    assert_eq!(plan.summary.canonical_objects, 1);
    assert_eq!(plan.summary.live_legacy_aliases, 2);
    assert_eq!(plan.summary.deleted_legacy_aliases, 2);
    assert_eq!(plan.summary.legacy_json_aliases, 1);

    let store = local_object_store(&fixture.object_root).unwrap();
    upload_plan_objects(&plan, store.clone(), 2).await.unwrap();
    upload_plan_objects(&plan, store, 2).await.unwrap();

    let stored = fs::read(
        fixture
            .object_root
            .join("legacy")
            .join("objects")
            .join(&fixture.blob.blob_id[..2])
            .join(format!("{}.nsb2", fixture.blob.blob_id)),
    )
    .unwrap();
    assert_eq!(stored, fixture.blob.storage_bytes().unwrap());
}

struct Fixture {
    root: PathBuf,
    primary_database: PathBuf,
    secondary_database: PathBuf,
    primary_media: PathBuf,
    secondary_media: PathBuf,
    object_root: PathBuf,
    blob: EncryptedBlob,
}

impl Fixture {
    fn new() -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "noise-migration-importer-{}-{sequence}",
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
        let object_root = root.join("objects");
        let (blob, _) = EncryptedBlob::create(b"encrypted migration fixture").unwrap();
        create_snapshot(
            &primary_database,
            &primary_media,
            &blob.storage_bytes().unwrap(),
            "a",
            "c",
        );
        create_snapshot(
            &secondary_database,
            &secondary_media,
            &serde_json::to_vec(&blob).unwrap(),
            "b",
            "d",
        );
        Self {
            root,
            primary_database,
            secondary_database,
            primary_media,
            secondary_media,
            object_root,
            blob,
        }
    }

    fn sources(&self) -> Vec<ImportSource> {
        vec![
            ImportSource {
                input: SourceInput {
                    label: "primary".into(),
                    database_path: self.primary_database.clone(),
                    media_root: self.primary_media.clone(),
                },
                compatibility_origin: "https://primary.example".into(),
            },
            ImportSource {
                input: SourceInput {
                    label: "secondary".into(),
                    database_path: self.secondary_database.clone(),
                    media_root: self.secondary_media.clone(),
                },
                compatibility_origin: "https://secondary.example".into(),
            },
        ]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn create_snapshot(
    database_path: &Path,
    media_root: &Path,
    payload: &[u8],
    shard_prefix: &str,
    tombstone_prefix: &str,
) {
    let shard_id = shard_prefix.repeat(64);
    let tombstone_id = tombstone_prefix.repeat(64);
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
    database
        .execute(
            "INSERT INTO relay_shards (
                shard_id, payload_hash, delete_token_hash, byte_length
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                shard_id,
                blake3::hash(payload).to_hex().to_string(),
                "e".repeat(64),
                payload.len() as i64,
            ],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO relay_shard_tombstones (shard_id) VALUES (?1)",
            params![tombstone_id],
        )
        .unwrap();
    drop(database);

    let directory = media_root.join("shards").join(&shard_id[..2]);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(format!("{shard_id}.bin")), payload).unwrap();
}
