use std::{env, fs, path::Path as FilePath, sync::Arc};

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use noise_core::StorageShard;
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, aws::AmazonS3Builder,
    local::LocalFileSystem, path::Path as ObjectPath,
};

const DEFAULT_S3_PREFIX: &str = "noise-relay";

#[derive(Clone)]
pub struct ShardStore {
    primary: Arc<dyn ObjectStore>,
    legacy_local: Option<Arc<dyn ObjectStore>>,
    prefix: Arc<str>,
    description: Arc<str>,
}

impl ShardStore {
    pub fn open(data_directory: &FilePath) -> anyhow::Result<Self> {
        let (local, local_directory) = open_local_store(data_directory)?;

        let configured_backend = env::var("NOISE_STORAGE_BACKEND")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        let bucket = env::var("NOISE_S3_BUCKET")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let use_s3 = match configured_backend.as_deref() {
            None => bucket.is_some(),
            Some("local") => false,
            Some("s3") => true,
            Some(other) => {
                bail!(
                    "unsupported NOISE_STORAGE_BACKEND value {other:?}; expected \"local\" or \"s3\""
                )
            }
        };

        if !use_s3 {
            return Ok(Self {
                primary: local,
                legacy_local: None,
                prefix: Arc::from(""),
                description: Arc::from(format!("local disk at {}", local_directory.display())),
            });
        }

        let bucket = bucket
            .context("NOISE_S3_BUCKET is required when NOISE_STORAGE_BACKEND is set to \"s3\"")?;
        let prefix = env::var("NOISE_S3_PREFIX")
            .unwrap_or_else(|_| DEFAULT_S3_PREFIX.to_owned())
            .trim_matches('/')
            .to_owned();
        if !prefix.is_empty() {
            ObjectPath::parse(&prefix).context("NOISE_S3_PREFIX is not a valid object prefix")?;
        }

        let s3: Arc<dyn ObjectStore> = Arc::new(
            AmazonS3Builder::from_env()
                .with_bucket_name(&bucket)
                // Individual deletes are more widely supported by S3-compatible
                // services than the bulk-delete API.
                .with_disable_bulk_delete(true)
                .build()
                .context("could not initialize S3-compatible encrypted media storage")?,
        );
        let location = if prefix.is_empty() {
            format!("S3-compatible bucket {bucket}")
        } else {
            format!("S3-compatible bucket {bucket} under {prefix}/")
        };
        Ok(Self {
            primary: s3,
            // If an operator moves an existing relay from local disk to S3,
            // reads lazily promote its old objects instead of breaking them.
            legacy_local: Some(local),
            prefix: Arc::from(prefix),
            description: Arc::from(location),
        })
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub async fn put_shard(&self, shard: &StorageShard) -> anyhow::Result<u64> {
        shard
            .verify()
            .context("storage shard failed integrity verification")?;
        let payload = STANDARD_NO_PAD
            .decode(&shard.payload_base64)
            .context("storage shard payload encoding is invalid")?;
        let byte_length = payload.len() as u64;
        let path = self.primary_shard_path(&shard.shard_id)?;
        self.primary
            .put(&path, payload.into())
            .await
            .with_context(|| format!("could not store shard {}", shard.shard_id))?;
        Ok(byte_length)
    }

    pub async fn get_shard(
        &self,
        shard_id: &str,
        payload_hash: &str,
        delete_token_hash: &str,
        byte_length: u64,
    ) -> anyhow::Result<Option<StorageShard>> {
        let primary_path = self.primary_shard_path(shard_id)?;
        let payload = match read_shard_bytes(
            self.primary.as_ref(),
            &primary_path,
            shard_id,
            payload_hash,
            byte_length,
        )
        .await
        {
            Ok(payload) => payload,
            Err(ReadError::Missing) => {
                let Some(local) = self.legacy_local.as_deref() else {
                    return Ok(None);
                };
                let local_path = relative_shard_path(shard_id)?;
                match read_shard_bytes(local, &local_path, shard_id, payload_hash, byte_length)
                    .await
                {
                    Ok(payload) => {
                        if let Err(error) = self
                            .primary
                            .put(&primary_path, payload.clone().into())
                            .await
                        {
                            eprintln!(
                                "could not promote storage shard {shard_id} to configured object storage: {error}"
                            );
                        }
                        payload
                    }
                    Err(ReadError::Missing) => return Ok(None),
                    Err(ReadError::Storage(error)) => return Err(error),
                }
            }
            Err(ReadError::Storage(error)) => return Err(error),
        };
        let shard = StorageShard {
            shard_id: shard_id.to_owned(),
            payload_hash: payload_hash.to_owned(),
            delete_token_hash: delete_token_hash.to_owned(),
            payload_base64: STANDARD_NO_PAD.encode(payload),
        };
        shard
            .verify()
            .context("stored shard failed integrity verification")?;
        Ok(Some(shard))
    }

    pub async fn delete_shard(&self, shard_id: &str) -> anyhow::Result<()> {
        let primary_path = self.primary_shard_path(shard_id)?;
        let primary_result = delete_if_present(self.primary.as_ref(), &primary_path).await;
        let local_result = if let Some(local) = self.legacy_local.as_deref() {
            let local_path = relative_shard_path(shard_id)?;
            delete_if_present(local, &local_path).await
        } else {
            Ok(())
        };
        primary_result.with_context(|| format!("could not delete storage shard {shard_id}"))?;
        local_result.with_context(|| format!("could not delete local storage shard {shard_id}"))?;
        Ok(())
    }

    pub async fn delete_legacy_blob(&self, blob_id: &str) -> anyhow::Result<()> {
        let primary_path = self.primary_object_path(blob_id)?;
        let primary_result = delete_if_present(self.primary.as_ref(), &primary_path).await;
        let local_result = if let Some(local) = self.legacy_local.as_deref() {
            let local_path = relative_object_path(blob_id)?;
            delete_if_present(local, &local_path).await
        } else {
            Ok(())
        };
        primary_result.with_context(|| format!("could not delete encrypted blob {blob_id}"))?;
        local_result
            .with_context(|| format!("could not delete legacy local encrypted blob {blob_id}"))?;
        Ok(())
    }

    fn primary_object_path(&self, blob_id: &str) -> anyhow::Result<ObjectPath> {
        let relative = relative_object_path(blob_id)?;
        let full = if self.prefix.is_empty() {
            relative.to_string()
        } else {
            format!("{}/{relative}", self.prefix)
        };
        ObjectPath::parse(full).context("encrypted blob produced an invalid object path")
    }

    fn primary_shard_path(&self, shard_id: &str) -> anyhow::Result<ObjectPath> {
        let relative = relative_shard_path(shard_id)?;
        let full = if self.prefix.is_empty() {
            relative.to_string()
        } else {
            format!("{}/{relative}", self.prefix)
        };
        ObjectPath::parse(full).context("storage shard produced an invalid object path")
    }
}

fn relative_object_path(blob_id: &str) -> anyhow::Result<ObjectPath> {
    validate_blob_id(blob_id)?;
    let shard = &blob_id[..2];
    ObjectPath::parse(format!("blobs/{shard}/{blob_id}.json"))
        .context("encrypted blob produced an invalid object path")
}

fn relative_shard_path(shard_id: &str) -> anyhow::Result<ObjectPath> {
    validate_blob_id(shard_id)?;
    let prefix = &shard_id[..2];
    ObjectPath::parse(format!("shards/{prefix}/{shard_id}.bin"))
        .context("storage shard produced an invalid object path")
}

fn open_local_store(
    data_directory: &FilePath,
) -> anyhow::Result<(Arc<dyn ObjectStore>, std::path::PathBuf)> {
    let shard_directory = data_directory.join("shards");
    fs::create_dir_all(&shard_directory).with_context(|| {
        format!(
            "could not create storage shard directory {}",
            shard_directory.display()
        )
    })?;
    let local: Arc<dyn ObjectStore> = Arc::new(
        LocalFileSystem::new_with_prefix(data_directory)
            .with_context(|| {
                format!(
                    "could not open relay object directory {}",
                    data_directory.display()
                )
            })?
            .with_automatic_cleanup(true)
            .with_fsync(true),
    );
    Ok((local, shard_directory))
}

enum ReadError {
    Missing,
    Storage(anyhow::Error),
}

async fn read_shard_bytes(
    store: &dyn ObjectStore,
    path: &ObjectPath,
    shard_id: &str,
    payload_hash: &str,
    byte_length: u64,
) -> Result<Vec<u8>, ReadError> {
    let result = match store.get(path).await {
        Ok(result) => result,
        Err(ObjectStoreError::NotFound { .. }) => return Err(ReadError::Missing),
        Err(error) => {
            return Err(ReadError::Storage(
                anyhow!(error).context(format!("could not read storage shard {shard_id}")),
            ));
        }
    };
    let bytes = result.bytes().await.map_err(|error| {
        ReadError::Storage(
            anyhow!(error).context(format!("could not download storage shard {shard_id}")),
        )
    })?;
    if bytes.len() as u64 != byte_length || blake3::hash(&bytes).to_hex().as_str() != payload_hash {
        return Err(ReadError::Storage(anyhow!(
            "storage shard {shard_id} failed integrity verification"
        )));
    }
    Ok(bytes.to_vec())
}

async fn delete_if_present(store: &dyn ObjectStore, path: &ObjectPath) -> object_store::Result<()> {
    match store.delete(path).await {
        Ok(()) | Err(ObjectStoreError::NotFound { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_blob_id(blob_id: &str) -> anyhow::Result<()> {
    if blob_id.len() != 64 || !blob_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid encrypted blob id")
    }
    Ok(())
}
