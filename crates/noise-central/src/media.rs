use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{
        HeaderMap, StatusCode,
        header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use noise_core::EncryptedBlob;
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, PutPayload, aws::AmazonS3Builder,
    path::Path as ObjectPath,
};

use crate::{
    AppState, authenticate_session,
    config::{CentralConfig, MediaConfig},
    decode_hex_32,
    error::ApiError,
};

const MAX_MEDIA_OBJECT_BYTES: usize = 2_100_000;

#[derive(Clone)]
pub(crate) struct MediaStore {
    objects: Arc<dyn ObjectStore>,
}

impl MediaStore {
    pub(crate) fn open(config: &CentralConfig) -> anyhow::Result<Option<Self>> {
        let Some(config) = config.media_config()? else {
            return Ok(None);
        };
        Self::from_config(config).map(Some)
    }

    fn from_config(config: MediaConfig) -> anyhow::Result<Self> {
        let objects: Arc<dyn ObjectStore> = Arc::new(
            AmazonS3Builder::new()
                .with_endpoint(config.endpoint)
                .with_region("auto")
                .with_bucket_name(config.bucket)
                .with_access_key_id(config.access_key_id)
                .with_secret_access_key(config.secret_access_key)
                .with_virtual_hosted_style_request(false)
                .with_disable_bulk_delete(true)
                .build()
                .context("could not initialize private R2 media storage")?,
        );
        Ok(Self { objects })
    }
}

pub(crate) async fn put_media(
    State(state): State<Arc<AppState>>,
    Path(object_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let media = state
        .media
        .as_ref()
        .ok_or_else(ApiError::service_unavailable)?;
    let session = authenticate_session(&state, &headers).await?;
    let object_id_bytes = decode_hex_32(&object_id, "invalid_media_id")?;
    if body.is_empty() || body.len() > MAX_MEDIA_OBJECT_BYTES {
        return Err(ApiError::bad_request("invalid_media_object"));
    }
    let blob = EncryptedBlob::from_storage_bytes(&body)
        .map_err(|_| ApiError::bad_request("invalid_media_object"))?;
    if blob.blob_id != object_id {
        return Err(ApiError::bad_request("media_id_mismatch"));
    }
    let storage_hash = *blake3::hash(&body).as_bytes();
    let storage_key = canonical_storage_key(&object_id);

    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    if let Some(existing) = client
        .query_opt(
            "SELECT owner_account_id, state, ciphertext_length, ciphertext_hash
             FROM noise.media_objects
             WHERE media_object_id = $1",
            &[&object_id_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        let owner_account_id: i64 = existing.get(0);
        let object_state: &str = existing.get(1);
        let length: i64 = existing.get(2);
        let hash: Vec<u8> = existing.get(3);
        if owner_account_id != session.account_id {
            return Err(ApiError::conflict("media_id_conflict"));
        }
        if object_state == "available" && length == body.len() as i64 && hash == storage_hash {
            return Ok(empty_response(StatusCode::OK));
        }
        return Err(ApiError::conflict("media_id_conflict"));
    }

    let location =
        ObjectPath::parse(&storage_key).map_err(|error| ApiError::database(anyhow!(error)))?;
    media
        .objects
        .put(&location, PutPayload::from_bytes(body.clone()))
        .await
        .map_err(|error| ApiError::database(anyhow!(error)))?;

    let mut deletion_input = b"noise.central.media.owner.v1".to_vec();
    deletion_input.extend_from_slice(&session.account_id.to_be_bytes());
    deletion_input.extend_from_slice(&object_id_bytes);
    let deletion_hash = *blake3::hash(&deletion_input).as_bytes();
    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.media_objects (
                media_object_id, owner_account_id, state, ciphertext_length,
                ciphertext_hash, deletion_capability_hash, finalized_at
             ) VALUES ($1, $2, 'available', $3, $4, $5, clock_timestamp())",
            &[
                &object_id_bytes.as_slice(),
                &session.account_id,
                &(body.len() as i64),
                &storage_hash.as_slice(),
                &deletion_hash.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.media_blocks (
                media_object_id, block_index, storage_key, ciphertext_length,
                ciphertext_hash, state, completed_at
             ) VALUES ($1, 0, $2, $3, $4, 'available', clock_timestamp())",
            &[
                &object_id_bytes.as_slice(),
                &storage_key,
                &(body.len() as i64),
                &storage_hash.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(empty_response(StatusCode::CREATED))
}

pub(crate) async fn get_media(
    State(state): State<Arc<AppState>>,
    Path(object_id): Path<String>,
) -> Result<Response, ApiError> {
    let media = state
        .media
        .as_ref()
        .ok_or_else(ApiError::service_unavailable)?;
    let object_id_bytes = decode_hex_32(&object_id, "invalid_media_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let record = if let Some(row) = client
        .query_opt(
            "SELECT b.storage_key, b.ciphertext_length, b.ciphertext_hash
             FROM noise.media_objects o
             JOIN noise.media_blocks b
               ON b.media_object_id = o.media_object_id AND b.block_index = 0
             LEFT JOIN noise.media_restrictions r
               ON r.media_object_id = o.media_object_id
             WHERE o.media_object_id = $1
               AND o.state = 'available'
               AND b.state = 'available'
               AND r.media_object_id IS NULL",
            &[&object_id_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        MediaRecord::from_row(row)?
    } else {
        let row = client
            .query_opt(
                "SELECT storage_key, storage_byte_length, storage_payload_hash
                 FROM noise.legacy_media_objects
                 WHERE media_object_id = $1 AND state = 'available'",
                &[&object_id_bytes.as_slice()],
            )
            .await
            .map_err(ApiError::database)?
            .ok_or_else(|| ApiError::not_found("media_not_found"))?;
        MediaRecord::from_row(row)?
    };
    let location = ObjectPath::parse(&record.storage_key)
        .map_err(|error| ApiError::database(anyhow!(error)))?;
    let stored = match media.objects.get(&location).await {
        Ok(result) => result
            .bytes()
            .await
            .map_err(|error| ApiError::database(anyhow!(error)))?,
        Err(ObjectStoreError::NotFound { .. }) => {
            return Err(ApiError::not_found("media_not_found"));
        }
        Err(error) => return Err(ApiError::database(anyhow!(error))),
    };
    if stored.len() != record.length || blake3::hash(&stored).as_bytes() != &record.hash {
        return Err(ApiError::database(anyhow!(
            "canonical media failed integrity verification"
        )));
    }
    Ok((
        StatusCode::OK,
        [
            (CONTENT_TYPE, "application/octet-stream"),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (CONTENT_LENGTH, &stored.len().to_string()),
        ],
        Body::from(stored),
    )
        .into_response())
}

pub(crate) async fn delete_media(
    State(state): State<Arc<AppState>>,
    Path(object_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let media = state
        .media
        .as_ref()
        .ok_or_else(ApiError::service_unavailable)?;
    let session = authenticate_session(&state, &headers).await?;
    let object_id_bytes = decode_hex_32(&object_id, "invalid_media_id")?;
    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let row = transaction
        .query_opt(
            "SELECT o.state, b.storage_key
             FROM noise.media_objects o
             JOIN noise.media_blocks b
               ON b.media_object_id = o.media_object_id AND b.block_index = 0
             WHERE o.media_object_id = $1 AND o.owner_account_id = $2
             FOR UPDATE OF o, b",
            &[&object_id_bytes.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("media_not_found"))?;
    let object_state: &str = row.get(0);
    let storage_key: &str = row.get(1);
    if object_state == "deleted" {
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok(empty_response(StatusCode::NO_CONTENT));
    }
    let location =
        ObjectPath::parse(storage_key).map_err(|error| ApiError::database(anyhow!(error)))?;
    match media.objects.delete(&location).await {
        Ok(()) | Err(ObjectStoreError::NotFound { .. }) => {}
        Err(error) => return Err(ApiError::database(anyhow!(error))),
    }
    transaction
        .execute(
            "UPDATE noise.media_blocks
             SET state = 'deleted', completed_at = clock_timestamp()
             WHERE media_object_id = $1",
            &[&object_id_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    transaction
        .execute(
            "UPDATE noise.media_objects
             SET state = 'deleted', deleted_at = clock_timestamp()
             WHERE media_object_id = $1",
            &[&object_id_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(empty_response(StatusCode::NO_CONTENT))
}

struct MediaRecord {
    storage_key: String,
    length: usize,
    hash: [u8; 32],
}

impl MediaRecord {
    fn from_row(row: tokio_postgres::Row) -> Result<Self, ApiError> {
        let length: i64 = row.get(1);
        if length <= 0 || length as usize > MAX_MEDIA_OBJECT_BYTES {
            return Err(ApiError::database(anyhow!(
                "invalid canonical media length"
            )));
        }
        let hash: Vec<u8> = row.get(2);
        Ok(Self {
            storage_key: row.get(0),
            length: length as usize,
            hash: hash
                .try_into()
                .map_err(|_| ApiError::database(anyhow!("invalid canonical media hash")))?,
        })
    }
}

fn canonical_storage_key(object_id: &str) -> String {
    format!("objects/{}/{object_id}.nsb2", &object_id[..2])
}

fn empty_response(status: StatusCode) -> Response {
    (
        status,
        [(CACHE_CONTROL, "no-store"), (CONTENT_LENGTH, "0")],
        Body::empty(),
    )
        .into_response()
}
