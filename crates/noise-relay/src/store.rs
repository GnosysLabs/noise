use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, anyhow};
use noise_core::{EncryptedBlob, GroupDeletion, InviteRecord, SignedEvent};
use serde::Serialize;
use turso::{Builder, Connection, params};

pub struct RecoveredState {
    pub invites: HashMap<String, InviteRecord>,
    pub events: HashMap<String, SignedEvent>,
    pub blobs: HashMap<String, EncryptedBlob>,
    pub deletions: HashMap<String, GroupDeletion>,
}

#[derive(Clone)]
pub struct DurableStore {
    connection: Connection,
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
             );",
            )
            .await?;

        let recovered = recover(&connection).await?;
        Ok((
            Self {
                connection,
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
        let changed = self
            .connection
            .execute(
                "INSERT OR IGNORE INTO relay_objects (kind, object_id, payload)
                 VALUES (?1, ?2, ?3)",
                params![kind, object_id, payload],
            )
            .await?;
        Ok(changed == 1)
    }

    pub async fn apply_group_deletion(
        &self,
        deletion: &GroupDeletion,
        invite_ids: &[String],
        event_ids: &[String],
        blob_ids: &[String],
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(deletion)?;
        self.connection.execute_batch("BEGIN IMMEDIATE;").await?;
        let result = async {
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO relay_objects (kind, object_id, payload)
                     VALUES (?1, ?2, ?3)",
                    params!["deletion", deletion.group_id.clone(), payload],
                )
                .await?;
            for (kind, object_ids) in [
                ("invite", invite_ids),
                ("event", event_ids),
                ("blob", blob_ids),
            ] {
                for object_id in object_ids {
                    self.connection
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
            let _ = self.connection.execute_batch("ROLLBACK;").await;
            return Err(error);
        }
        self.connection.execute_batch("COMMIT;").await?;
        Ok(())
    }
}

async fn recover(connection: &Connection) -> anyhow::Result<RecoveredState> {
    let mut invites = HashMap::new();
    let mut events = HashMap::new();
    let mut blobs = HashMap::new();
    let mut deletions = HashMap::new();
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
            "blob" => {
                let blob: EncryptedBlob = serde_json::from_str(&payload)
                    .with_context(|| format!("stored blob {object_id} is invalid JSON"))?;
                blob.verify()
                    .with_context(|| format!("stored blob {object_id} failed verification"))?;
                if blob.blob_id != object_id {
                    return Err(anyhow!("stored blob key does not match its blob id"));
                }
                blobs.insert(object_id, blob);
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
    events.retain(|_, event| !deletions.contains_key(&event.group_id));
    blobs.retain(|_, blob| {
        blob.group_id
            .as_ref()
            .is_none_or(|group_id| !deletions.contains_key(group_id))
    });

    Ok(RecoveredState {
        invites,
        events,
        blobs,
        deletions,
    })
}
