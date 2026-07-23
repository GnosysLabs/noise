use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, anyhow};
use noise_core::{
    AccountVault, EncryptedBlob, GroupDeletion, InviteRecord, InviteRotation, MlsEpochRecord,
    MlsGroupGenesis, MlsJoinRequest, MlsRemovalRequest, SignedEvent, StorageShard,
};
use noise_transport::SignedRelayDescriptor;
use serde::Serialize;
use tokio::sync::Mutex;
use turso::{Builder, Connection, params};

#[derive(Clone, Debug)]
pub struct LegacyBlobMetadata {
    pub blob_id: String,
}

#[derive(Clone, Debug)]
pub struct ShardMetadata {
    pub payload_hash: String,
    pub delete_token_hash: String,
    pub byte_length: u64,
}

pub struct RecoveredState {
    pub accounts: HashMap<String, AccountVault>,
    pub invites: HashMap<String, InviteRecord>,
    pub invite_rotations: HashMap<String, InviteRotation>,
    pub events: HashMap<String, SignedEvent>,
    pub mls_join_requests: HashMap<String, MlsJoinRequest>,
    pub mls_removal_requests: HashMap<String, MlsRemovalRequest>,
    pub mls_geneses: HashMap<String, MlsGroupGenesis>,
    pub mls_epochs: HashMap<String, MlsEpochRecord>,
    pub blobs: Vec<String>,
    pub legacy_blobs: Vec<LegacyBlobMetadata>,
    pub pending_blob_deletions: Vec<String>,
    pub legacy_blob_schema: bool,
    pub pending_shard_deletions: Vec<String>,
    pub shard_count: u64,
    pub shard_bytes: u64,
    pub deletions: HashMap<String, GroupDeletion>,
    pub relay_descriptors: HashMap<String, SignedRelayDescriptor>,
}

#[derive(Clone)]
pub struct DurableStore {
    connection: Arc<Mutex<Connection>>,
    path: Arc<PathBuf>,
}

impl DurableStore {
    pub async fn open(data_directory: &Path) -> anyhow::Result<(Self, RecoveredState)> {
        fs::create_dir_all(data_directory).with_context(|| {
            format!(
                "could not create relay data directory {}",
                data_directory.display()
            )
        })?;
        let path = data_directory.join("relay.db");
        let path_string = path.to_string_lossy().into_owned();
        let database = Builder::new_local(&path_string)
            // Used only for the one-time rewrite that physically removes
            // legacy inline media pages after migration.
            .experimental_vacuum(true)
            .build()
            .await
            .with_context(|| format!("could not open relay database {}", path.display()))?;
        let connection = database.connect()?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS relay_objects (
                 kind TEXT NOT NULL,
                 object_id TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 PRIMARY KEY (kind, object_id)
             );
             CREATE TABLE IF NOT EXISTS relay_directory (
                 relay_id TEXT PRIMARY KEY,
                 descriptor TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relay_shards (
                 shard_id TEXT PRIMARY KEY,
                 payload_hash TEXT NOT NULL,
                 delete_token_hash TEXT NOT NULL,
                 byte_length INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relay_shard_delete_queue (
                 shard_id TEXT PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS relay_shard_tombstones (
                 shard_id TEXT PRIMARY KEY
             );",
            )
            .await?;

        let mut recovered = recover(&connection).await?;
        recovered.relay_descriptors = recover_relay_descriptors(&connection).await?;
        Ok((
            Self {
                connection: Arc::new(Mutex::new(connection)),
                path: Arc::new(path),
            },
            recovered,
        ))
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub async fn insert<T: Serialize>(
        &self,
        kind: &'static str,
        object_id: String,
        object: &T,
    ) -> anyhow::Result<bool> {
        let payload = serde_json::to_string(object)?;
        let connection = self.connection.lock().await;
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO relay_objects (kind, object_id, payload)
                 VALUES (?1, ?2, ?3)",
                params![kind, object_id, payload],
            )
            .await?;
        Ok(changed == 1)
    }

    pub async fn shard_metadata(&self, shard_id: &str) -> anyhow::Result<Option<ShardMetadata>> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT payload_hash, delete_token_hash, byte_length
                 FROM relay_shards WHERE shard_id = ?1 LIMIT 1",
                params![shard_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let byte_length: i64 = row.get(2)?;
        Ok(Some(ShardMetadata {
            payload_hash: row.get(0)?,
            delete_token_hash: row.get(1)?,
            byte_length: byte_length
                .try_into()
                .context("stored shard has an invalid byte length")?,
        }))
    }

    pub async fn shard_was_deleted(&self, shard_id: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT 1 FROM relay_shard_tombstones
                 WHERE shard_id = ?1 LIMIT 1",
                params![shard_id],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    pub async fn insert_shard(
        &self,
        shard: &StorageShard,
        byte_length: u64,
    ) -> anyhow::Result<bool> {
        let connection = self.connection.lock().await;
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO relay_shards
                 (shard_id, payload_hash, delete_token_hash, byte_length)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    shard.shard_id.clone(),
                    shard.payload_hash.clone(),
                    shard.delete_token_hash.clone(),
                    i64::try_from(byte_length).context("storage shard is too large")?
                ],
            )
            .await?;
        Ok(changed == 1)
    }

    pub async fn queue_shard_deletion(&self, shard_id: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().await;
        connection.execute_batch("BEGIN IMMEDIATE;").await?;
        let result = async {
            let changed = connection
                .execute(
                    "DELETE FROM relay_shards WHERE shard_id = ?1",
                    params![shard_id],
                )
                .await?;
            if changed == 1 {
                connection
                    .execute(
                        "INSERT OR IGNORE INTO relay_shard_tombstones (shard_id)
                         VALUES (?1)",
                        params![shard_id],
                    )
                    .await?;
                connection
                    .execute(
                        "INSERT OR IGNORE INTO relay_shard_delete_queue (shard_id)
                         VALUES (?1)",
                        params![shard_id],
                    )
                    .await?;
            }
            Ok::<bool, anyhow::Error>(changed == 1)
        }
        .await;
        match result {
            Ok(changed) => {
                connection.execute_batch("COMMIT;").await?;
                Ok(changed)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK;").await;
                Err(error)
            }
        }
    }

    pub async fn pending_shard_deletions(&self) -> anyhow::Result<Vec<String>> {
        let mut pending = Vec::new();
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT shard_id FROM relay_shard_delete_queue ORDER BY shard_id",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            pending.push(row.get(0)?);
        }
        Ok(pending)
    }

    pub async fn complete_shard_deletion(&self, shard_id: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "DELETE FROM relay_shard_delete_queue WHERE shard_id = ?1",
                params![shard_id],
            )
            .await?;
        Ok(())
    }

    pub async fn discard_all_legacy_blobs(&self, blob_ids: &[String]) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection.execute_batch("BEGIN IMMEDIATE;").await?;
        let result = async {
            for blob_id in blob_ids {
                connection
                    .execute(
                        "DELETE FROM relay_objects WHERE kind = 'blob' AND object_id = ?1",
                        params![blob_id.clone()],
                    )
                    .await?;
            }
            connection
                .execute_batch(
                    "DROP TABLE IF EXISTS relay_blob_delete_queue;
                     DROP TABLE IF EXISTS relay_blobs;",
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let _ = connection.execute_batch("ROLLBACK;").await;
            return Err(error);
        }
        connection.execute_batch("COMMIT;").await?;
        Ok(())
    }

    pub async fn reclaim_inline_blob_space(&self) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute_batch("VACUUM;")
            .await
            .context("could not compact relay database after encrypted media migration")
    }

    pub async fn upsert_account(&self, vault: &AccountVault) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "INSERT INTO relay_objects (kind, object_id, payload)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(kind, object_id) DO UPDATE SET payload = excluded.payload",
                params![
                    "account",
                    vault.locator.clone(),
                    serde_json::to_string(vault)?
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn upsert_relay_descriptor(
        &self,
        descriptor: &SignedRelayDescriptor,
    ) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection
            .execute(
                "INSERT INTO relay_directory (relay_id, descriptor)
                 VALUES (?1, ?2)
                 ON CONFLICT(relay_id) DO UPDATE SET descriptor = excluded.descriptor",
                params![
                    descriptor.relay_id.clone(),
                    serde_json::to_string(descriptor)?
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn apply_group_deletion(
        &self,
        deletion: &GroupDeletion,
        invite_ids: &[String],
        event_ids: &[String],
        mls_join_request_ids: &[String],
        mls_removal_request_ids: &[String],
        mls_epoch_ids: &[String],
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(deletion)?;
        let connection = self.connection.lock().await;
        connection.execute_batch("BEGIN IMMEDIATE;").await?;
        let result = async {
            connection
                .execute(
                    "INSERT OR IGNORE INTO relay_objects (kind, object_id, payload)
                     VALUES (?1, ?2, ?3)",
                    params!["deletion", deletion.group_id.clone(), payload],
                )
                .await?;
            connection
                .execute(
                    "DELETE FROM relay_objects WHERE kind = ?1 AND object_id = ?2",
                    params!["invite_rotation", deletion.group_id.clone()],
                )
                .await?;
            connection
                .execute(
                    "DELETE FROM relay_objects WHERE kind = ?1 AND object_id = ?2",
                    params!["mls_genesis", deletion.group_id.clone()],
                )
                .await?;
            for (kind, object_ids) in [
                ("invite", invite_ids),
                ("event", event_ids),
                ("mls_join_request", mls_join_request_ids),
                ("mls_removal_request", mls_removal_request_ids),
                ("mls_epoch", mls_epoch_ids),
            ] {
                for object_id in object_ids {
                    connection
                        .execute(
                            "DELETE FROM relay_objects WHERE kind = ?1 AND object_id = ?2",
                            params![kind, object_id.clone()],
                        )
                        .await?;
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let _ = connection.execute_batch("ROLLBACK;").await;
            return Err(error);
        }
        connection.execute_batch("COMMIT;").await?;
        Ok(())
    }

    pub async fn apply_invite_rotation(
        &self,
        rotation: &InviteRotation,
        invite_ids: &[String],
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(rotation)?;
        let connection = self.connection.lock().await;
        connection.execute_batch("BEGIN IMMEDIATE;").await?;
        let result = async {
            connection
                .execute(
                    "INSERT INTO relay_objects (kind, object_id, payload)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(kind, object_id) DO UPDATE SET payload = excluded.payload",
                    params!["invite_rotation", rotation.group_id.clone(), payload],
                )
                .await?;
            for object_id in invite_ids {
                connection
                    .execute(
                        "DELETE FROM relay_objects WHERE kind = ?1 AND object_id = ?2",
                        params!["invite", object_id.clone()],
                    )
                    .await?;
            }
            if let Some(invite) = rotation.new_invite.as_ref() {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO relay_objects (kind, object_id, payload)
                         VALUES (?1, ?2, ?3)",
                        params![
                            "invite",
                            invite.locator.clone(),
                            serde_json::to_string(invite)?
                        ],
                    )
                    .await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let _ = connection.execute_batch("ROLLBACK;").await;
            return Err(error);
        }
        connection.execute_batch("COMMIT;").await?;
        Ok(())
    }
}

async fn recover(connection: &Connection) -> anyhow::Result<RecoveredState> {
    let mut accounts = HashMap::new();
    let mut invites = HashMap::new();
    let mut invite_rotations = HashMap::new();
    let mut events = HashMap::new();
    let mut mls_join_requests = HashMap::new();
    let mut mls_removal_requests = HashMap::new();
    let mut mls_geneses = HashMap::new();
    let mut mls_epochs = HashMap::new();
    let mut blobs = Vec::new();
    let mut legacy_blobs = Vec::new();
    let mut pending_blob_deletions = Vec::new();
    let mut pending_shard_deletions = Vec::new();
    let mut deletions = HashMap::new();
    let (shard_count, shard_bytes) = {
        let mut rows = connection
            .query(
                "SELECT COUNT(*), COALESCE(SUM(byte_length), 0) FROM relay_shards",
                (),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .context("shard totals query returned no row")?;
        let count: i64 = row.get(0)?;
        let bytes: i64 = row.get(1)?;
        (
            count.try_into().context("stored shard count is invalid")?,
            bytes
                .try_into()
                .context("stored shard byte total is invalid")?,
        )
    };

    let legacy_blob_table_exists = table_exists(connection, "relay_blobs").await?;
    let legacy_blob_queue_exists = table_exists(connection, "relay_blob_delete_queue").await?;
    if legacy_blob_table_exists {
        let mut blob_rows = connection
            .query("SELECT blob_id FROM relay_blobs ORDER BY blob_id", ())
            .await?;
        while let Some(row) = blob_rows.next().await? {
            let blob_id: String = row.get(0)?;
            blobs.push(blob_id);
        }
    }

    if legacy_blob_queue_exists {
        let mut deletion_rows = connection
            .query(
                "SELECT blob_id FROM relay_blob_delete_queue ORDER BY blob_id",
                (),
            )
            .await?;
        while let Some(row) = deletion_rows.next().await? {
            pending_blob_deletions.push(row.get(0)?);
        }
    }

    let mut shard_deletion_rows = connection
        .query(
            "SELECT shard_id FROM relay_shard_delete_queue ORDER BY shard_id",
            (),
        )
        .await?;
    while let Some(row) = shard_deletion_rows.next().await? {
        pending_shard_deletions.push(row.get(0)?);
    }
    drop(shard_deletion_rows);

    let mut rows = connection
        .query(
            "SELECT kind, object_id, payload
             FROM relay_objects
             ORDER BY rowid",
            (),
        )
        .await?;

    while let Some(row) = rows.next().await? {
        let kind: String = row.get(0)?;
        let object_id: String = row.get(1)?;
        let payload: String = row.get(2)?;
        match kind.as_str() {
            "account" => {
                let vault: AccountVault = serde_json::from_str(&payload)
                    .with_context(|| format!("stored account {object_id} is invalid JSON"))?;
                vault
                    .verify()
                    .with_context(|| format!("stored account {object_id} failed verification"))?;
                if vault.locator != object_id {
                    return Err(anyhow!("stored account key does not match its locator"));
                }
                accounts.insert(object_id, vault);
            }
            "invite" => {
                let record: InviteRecord = serde_json::from_str(&payload)
                    .with_context(|| format!("stored invitation {object_id} is invalid JSON"))?;
                record.verify().with_context(|| {
                    format!("stored invitation {object_id} failed verification")
                })?;
                if record.locator != object_id {
                    return Err(anyhow!("stored invitation key does not match its locator"));
                }
                invites.insert(object_id, record);
            }
            "invite_rotation" => {
                let rotation: InviteRotation =
                    serde_json::from_str(&payload).with_context(|| {
                        format!("stored invite rotation {object_id} is invalid JSON")
                    })?;
                rotation.verify().with_context(|| {
                    format!("stored invite rotation {object_id} failed verification")
                })?;
                if rotation.group_id != object_id {
                    return Err(anyhow!(
                        "stored invite rotation key does not match its group id"
                    ));
                }
                invite_rotations.insert(object_id, rotation);
            }
            "event" => {
                let event: SignedEvent = serde_json::from_str(&payload)
                    .with_context(|| format!("stored event {object_id} is invalid JSON"))?;
                event
                    .verify()
                    .with_context(|| format!("stored event {object_id} failed verification"))?;
                if event.event_id != object_id {
                    return Err(anyhow!("stored event key does not match its event id"));
                }
                events.insert(object_id, event);
            }
            "mls_join_request" => {
                let request: MlsJoinRequest =
                    serde_json::from_str(&payload).with_context(|| {
                        format!("stored MLS join request {object_id} is invalid JSON")
                    })?;
                request.verify().with_context(|| {
                    format!("stored MLS join request {object_id} failed verification")
                })?;
                if request.request_id != object_id {
                    return Err(anyhow!(
                        "stored MLS join request key does not match its request id"
                    ));
                }
                mls_join_requests.insert(object_id, request);
            }
            "mls_removal_request" => {
                let request: MlsRemovalRequest =
                    serde_json::from_str(&payload).with_context(|| {
                        format!("stored MLS removal request {object_id} is invalid JSON")
                    })?;
                request.verify().with_context(|| {
                    format!("stored MLS removal request {object_id} failed verification")
                })?;
                if request.request_id != object_id {
                    return Err(anyhow!(
                        "stored MLS removal request key does not match its request id"
                    ));
                }
                mls_removal_requests.insert(object_id, request);
            }
            "mls_genesis" => {
                let genesis: MlsGroupGenesis = serde_json::from_str(&payload)
                    .with_context(|| format!("stored MLS genesis {object_id} is invalid JSON"))?;
                genesis.verify().with_context(|| {
                    format!("stored MLS genesis {object_id} failed verification")
                })?;
                if genesis.group_id != object_id {
                    return Err(anyhow!(
                        "stored MLS genesis key does not match its group id"
                    ));
                }
                mls_geneses.insert(object_id, genesis);
            }
            "mls_epoch" => {
                let epoch: MlsEpochRecord = serde_json::from_str(&payload)
                    .with_context(|| format!("stored MLS epoch {object_id} is invalid JSON"))?;
                epoch
                    .verify()
                    .with_context(|| format!("stored MLS epoch {object_id} failed verification"))?;
                if epoch.record_id != object_id {
                    return Err(anyhow!("stored MLS epoch key does not match its record id"));
                }
                mls_epochs.insert(object_id, epoch);
            }
            "blob" => {
                let blob: EncryptedBlob = serde_json::from_str(&payload)
                    .with_context(|| format!("stored blob {object_id} is invalid JSON"))?;
                blob.verify()
                    .with_context(|| format!("stored blob {object_id} failed verification"))?;
                if blob.blob_id != object_id {
                    return Err(anyhow!("stored blob key does not match its blob id"));
                }
                legacy_blobs.push(LegacyBlobMetadata { blob_id: object_id });
            }
            "deletion" => {
                let deletion: GroupDeletion = serde_json::from_str(&payload)
                    .with_context(|| format!("stored deletion {object_id} is invalid JSON"))?;
                deletion
                    .verify()
                    .with_context(|| format!("stored deletion {object_id} failed verification"))?;
                if deletion.group_id != object_id {
                    return Err(anyhow!("stored deletion key does not match its group id"));
                }
                deletions.insert(object_id, deletion);
            }
            other => return Err(anyhow!("unknown stored relay object kind {other}")),
        }
    }

    invites.retain(|_, invite| {
        invite
            .group_id
            .as_ref()
            .is_none_or(|group_id| !deletions.contains_key(group_id))
    });
    invite_rotations.retain(|group_id, _| !deletions.contains_key(group_id));
    for rotation in invite_rotations.values() {
        invites.retain(|locator, invite| {
            invite.group_id.as_deref() != Some(rotation.group_id.as_str())
                || rotation
                    .new_invite
                    .as_ref()
                    .is_some_and(|current| current.locator == *locator)
        });
        if let Some(invite) = rotation.new_invite.as_ref() {
            invites.insert(invite.locator.clone(), invite.clone());
        }
    }
    events.retain(|_, event| !deletions.contains_key(&event.group_id));
    mls_join_requests.retain(|_, request| !deletions.contains_key(&request.group_id));
    mls_removal_requests.retain(|_, request| !deletions.contains_key(&request.group_id));
    mls_geneses.retain(|group_id, _| !deletions.contains_key(group_id));
    mls_epochs.retain(|_, epoch| !deletions.contains_key(&epoch.bundle.group_id));

    Ok(RecoveredState {
        accounts,
        invites,
        invite_rotations,
        events,
        mls_join_requests,
        mls_removal_requests,
        mls_geneses,
        mls_epochs,
        blobs,
        legacy_blobs,
        pending_blob_deletions,
        legacy_blob_schema: legacy_blob_table_exists || legacy_blob_queue_exists,
        pending_shard_deletions,
        shard_count,
        shard_bytes,
        deletions,
        relay_descriptors: HashMap::new(),
    })
}

async fn recover_relay_descriptors(
    connection: &Connection,
) -> anyhow::Result<HashMap<String, SignedRelayDescriptor>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    let mut descriptors = HashMap::new();
    let mut rows = connection
        .query(
            "SELECT relay_id, descriptor FROM relay_directory ORDER BY relay_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let relay_id: String = row.get(0)?;
        let payload: String = row.get(1)?;
        let Ok(descriptor) = serde_json::from_str::<SignedRelayDescriptor>(&payload) else {
            continue;
        };
        if descriptor.relay_id == relay_id && descriptor.verify_at(now).is_ok() {
            descriptors.insert(relay_id, descriptor);
        }
    }
    Ok(descriptors)
}

async fn table_exists(connection: &Connection, table: &str) -> anyhow::Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = ?1
             LIMIT 1",
            params![table],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}
