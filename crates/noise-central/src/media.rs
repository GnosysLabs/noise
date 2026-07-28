use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use noise_core::EncryptedBlob;
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, aws::AmazonS3Builder,
    path::Path as ObjectPath,
};

use crate::{
    AppState,
    config::{CentralConfig, LegacyMediaConfig},
};

const MAX_LEGACY_MEDIA_BYTES: usize = 2_100_000;

#[derive(Clone)]
pub(crate) struct LegacyMediaStore {
    objects: Arc<dyn ObjectStore>,
    compatibility_token_hash: [u8; 32],
}

struct LegacyAlias {
    payload_hash: [u8; 32],
    payload_length: usize,
    encoding: LegacyEncoding,
    storage_key: String,
    storage_hash: [u8; 32],
    storage_length: usize,
}

#[derive(Clone, Copy)]
enum LegacyEncoding {
    Nsb2,
    Json,
}

impl LegacyMediaStore {
    pub(crate) fn open(config: &CentralConfig) -> anyhow::Result<Option<Self>> {
        let Some(config) = config.legacy_media_config()? else {
            return Ok(None);
        };
        Ok(Some(Self::from_config(config)?))
    }

    fn from_config(config: LegacyMediaConfig) -> anyhow::Result<Self> {
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
                .context("could not initialize private R2 media reads")?,
        );
        Ok(Self {
            objects,
            compatibility_token_hash: config.compatibility_token_hash,
        })
    }
}

pub(crate) async fn get_legacy_shard(
    State(state): State<Arc<AppState>>,
    Path((provider, shard_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(media) = state.legacy_media.as_ref() else {
        return legacy_error(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !authorized(&headers, &media.compatibility_token_hash) {
        return legacy_error(StatusCode::UNAUTHORIZED);
    }
    let Ok(shard_id) = decode_hex_32(&shard_id) else {
        return legacy_error(StatusCode::BAD_REQUEST);
    };
    if provider.is_empty()
        || provider.len() > 64
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
    {
        return legacy_error(StatusCode::BAD_REQUEST);
    }

    match read_legacy_shard(&state, media, &provider, &shard_id).await {
        Ok(Some(payload)) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, "application/octet-stream"),
                (CACHE_CONTROL, "private, no-store"),
                (CONTENT_LENGTH, &payload.len().to_string()),
            ],
            Body::from(payload),
        )
            .into_response(),
        Ok(None) => legacy_error(StatusCode::NOT_FOUND),
        Err(_) => {
            eprintln!("noise-central legacy media read failed");
            legacy_error(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn read_legacy_shard(
    state: &AppState,
    media: &LegacyMediaStore,
    provider: &str,
    shard_id: &[u8; 32],
) -> anyhow::Result<Option<Vec<u8>>> {
    let client = state
        .database
        .pool
        .get()
        .await
        .context("could not acquire PostgreSQL connection")?;
    let Some(row) = client
        .query_opt(
            "SELECT
                shard.state,
                shard.payload_hash,
                shard.ciphertext_length,
                shard.payload_encoding,
                object.storage_key,
                object.storage_payload_hash,
                object.storage_byte_length,
                object.state
             FROM noise.legacy_media_providers provider
             JOIN noise.legacy_media_shards shard
               ON shard.provider_id = provider.provider_id
             LEFT JOIN noise.legacy_media_objects object
               ON object.media_object_id = shard.canonical_object_id
             WHERE provider.provider_name = $1
               AND shard.shard_id = $2",
            &[&provider, &shard_id.as_slice()],
        )
        .await
        .context("could not resolve legacy media alias")?
    else {
        return Ok(None);
    };

    let shard_state: &str = row.get(0);
    if shard_state == "deleted" {
        return Ok(None);
    }
    let object_state: Option<&str> = row.get(7);
    if shard_state != "live" || object_state != Some("available") {
        bail!("legacy media alias is not readable");
    }
    let alias = LegacyAlias {
        payload_hash: exact_32(row.get::<_, Option<Vec<u8>>>(1), "legacy payload hash")?,
        payload_length: positive_length(row.get::<_, Option<i64>>(2), "legacy ciphertext length")?,
        encoding: match row.get::<_, Option<&str>>(3) {
            Some("nsb2") => LegacyEncoding::Nsb2,
            Some("legacy_json") => LegacyEncoding::Json,
            _ => bail!("legacy media encoding is invalid"),
        },
        storage_key: row
            .get::<_, Option<&str>>(4)
            .context("canonical media object key is missing")?
            .to_owned(),
        storage_hash: exact_32(row.get::<_, Option<Vec<u8>>>(5), "canonical storage hash")?,
        storage_length: positive_length(row.get::<_, Option<i64>>(6), "canonical storage length")?,
    };
    let location =
        ObjectPath::parse(&alias.storage_key).context("canonical object key is invalid")?;
    let stored = match media.objects.get(&location).await {
        Ok(result) => result
            .bytes()
            .await
            .context("could not read canonical R2 object")?,
        Err(ObjectStoreError::NotFound { .. }) => {
            bail!("canonical R2 object is missing");
        }
        Err(error) => return Err(anyhow!(error).context("could not fetch canonical R2 object")),
    };
    if stored.len() != alias.storage_length
        || blake3::hash(&stored).as_bytes() != &alias.storage_hash
    {
        bail!("canonical R2 object failed integrity verification");
    }
    let payload = reconstruct_legacy_payload(&stored, alias.encoding)?;
    if payload.len() != alias.payload_length
        || payload.len() > MAX_LEGACY_MEDIA_BYTES
        || blake3::hash(&payload).as_bytes() != &alias.payload_hash
    {
        bail!("reconstructed legacy media failed integrity verification");
    }
    Ok(Some(payload))
}

fn reconstruct_legacy_payload(stored: &[u8], encoding: LegacyEncoding) -> anyhow::Result<Vec<u8>> {
    match encoding {
        LegacyEncoding::Nsb2 => Ok(stored.to_vec()),
        LegacyEncoding::Json => {
            let blob = EncryptedBlob::from_storage_bytes(stored)
                .context("canonical encrypted object is invalid")?;
            serde_json::to_vec(&blob).context("could not reconstruct legacy JSON media")
        }
    }
}

fn authorized(headers: &HeaderMap, expected_hash: &[u8; 32]) -> bool {
    let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    blake3::hash(token.as_bytes()).as_bytes() == expected_hash
}

fn decode_hex_32(value: &str) -> anyhow::Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("legacy shard ID is invalid");
    }
    let mut output = [0_u8; 32];
    for (index, output) in output.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(output)
}

fn exact_32(value: Option<Vec<u8>>, label: &str) -> anyhow::Result<[u8; 32]> {
    value
        .with_context(|| format!("{label} is missing"))?
        .try_into()
        .map_err(|_| anyhow!("{label} has an invalid length"))
}

fn positive_length(value: Option<i64>, label: &str) -> anyhow::Result<usize> {
    let value = value.with_context(|| format!("{label} is missing"))?;
    if value <= 0 {
        bail!("{label} is invalid");
    }
    usize::try_from(value).context("media length is too large")
}

fn legacy_error(status: StatusCode) -> Response {
    (
        status,
        [(CACHE_CONTROL, "no-store"), (CONTENT_LENGTH, "0")],
        Body::empty(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bytes_reproduce_both_legacy_encodings() {
        let (blob, _) = EncryptedBlob::create(b"legacy compatibility fixture").unwrap();
        let canonical = blob.storage_bytes().unwrap();

        assert_eq!(
            reconstruct_legacy_payload(&canonical, LegacyEncoding::Nsb2).unwrap(),
            canonical
        );
        assert_eq!(
            reconstruct_legacy_payload(&canonical, LegacyEncoding::Json).unwrap(),
            serde_json::to_vec(&blob).unwrap()
        );
    }

    #[test]
    fn shard_ids_must_be_canonical_lowercase_hex() {
        assert!(decode_hex_32(&"ab".repeat(32)).is_ok());
        assert!(decode_hex_32(&"AB".repeat(32)).is_err());
        assert!(decode_hex_32("not-a-shard").is_err());
    }
}
