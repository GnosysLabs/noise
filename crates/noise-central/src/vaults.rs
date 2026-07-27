use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, ETAG, IF_MATCH},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use noise_core::AccountVault;

use super::{
    AppState, authenticate_session, decode_canonical_array, decode_hex_32, error::ApiError,
    parse_u64,
};

pub async fn get_account_vault(
    State(state): State<Arc<AppState>>,
    Path(locator): Path<String>,
) -> Result<Response, ApiError> {
    let locator_bytes = decode_hex_32(&locator, "invalid_vault_locator")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_opt(
            "SELECT v.revision::text, v.signed_wire_record
             FROM noise.account_vault_locators l
             JOIN noise.account_vault_heads h ON h.locator = l.locator
             JOIN noise.account_vault_versions v
               ON v.locator = h.locator AND v.revision = h.revision
             WHERE l.locator = $1",
            &[&locator_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("vault_not_found"))?;
    let revision = parse_u64(row.get::<_, &str>(0))?;
    let wire_record: Vec<u8> = row.get(1);
    let vault: AccountVault = serde_json::from_slice(&wire_record).map_err(ApiError::database)?;
    vault.verify().map_err(ApiError::database)?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(ETAG, etag(revision)?);
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((StatusCode::OK, response_headers, Json(vault)).into_response())
}

pub async fn put_account_vault(
    State(state): State<Arc<AppState>>,
    Path(locator): Path<String>,
    headers: HeaderMap,
    Json(vault): Json<AccountVault>,
) -> Result<Response, ApiError> {
    vault
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_account_vault"))?;
    if vault.locator != locator {
        return Err(ApiError::bad_request("vault_locator_mismatch"));
    }
    let expected_revision = if_match(&headers)?;
    if vault.revision
        != expected_revision
            .checked_add(1)
            .ok_or_else(|| ApiError::precondition_failed("vault_revision_must_advance_by_one"))?
    {
        return Err(ApiError::precondition_failed(
            "vault_revision_must_advance_by_one",
        ));
    }

    let session = authenticate_session(&state, &headers).await?;
    let locator_bytes = decode_hex_32(&locator, "invalid_vault_locator")?;
    let identity_public_key =
        decode_canonical_array::<32>(&vault.identity_public_key, "invalid_account_key")?;
    if identity_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("vault_identity_mismatch"));
    }
    let signature = decode_canonical_array::<64>(&vault.signature_base64, "invalid_account_vault")?;
    let nonce = if vault.deleted {
        None
    } else {
        Some(decode_canonical_array::<24>(&vault.nonce_base64, "invalid_account_vault")?.to_vec())
    };
    let ciphertext = if vault.deleted {
        None
    } else {
        Some(decode_canonical_vec(
            &vault.ciphertext_base64,
            "invalid_account_vault",
        )?)
    };
    let signed_wire_record = serde_json::to_vec(&vault).map_err(ApiError::database)?;
    let revision_text = vault.revision.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let account = transaction
        .query_one(
            "SELECT status
             FROM noise.accounts
             WHERE account_id = $1 AND identity_public_key = $2
             FOR UPDATE",
            &[&session.account_id, &identity_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    if account.get::<_, &str>(0) != "active" {
        return Err(ApiError::unauthorized());
    }

    let locator_owner = transaction
        .query_opt(
            "SELECT account_id
             FROM noise.account_vault_locators
             WHERE locator = $1
             FOR UPDATE",
            &[&locator_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    let is_new_locator = locator_owner.is_none();
    if let Some(owner) = locator_owner {
        if owner.get::<_, i64>(0) != session.account_id {
            return Err(ApiError::conflict("vault_locator_in_use"));
        }
    } else {
        if expected_revision != 0 {
            return Err(ApiError::precondition_failed("vault_revision_conflict"));
        }
        transaction
            .execute(
                "INSERT INTO noise.account_vault_locators (locator, account_id)
                 VALUES ($1, $2)",
                &[&locator_bytes.as_slice(), &session.account_id],
            )
            .await
            .map_err(vault_conflict)?;
    }

    let current = transaction
        .query_opt(
            "SELECT h.revision::text, v.deleted, v.signed_wire_record
             FROM noise.account_vault_heads h
             JOIN noise.account_vault_versions v
               ON v.locator = h.locator AND v.revision = h.revision
             WHERE h.locator = $1
             FOR UPDATE OF h",
            &[&locator_bytes.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    if let Some(current) = current {
        let current_revision = parse_u64(current.get::<_, &str>(0))?;
        let current_deleted: bool = current.get(1);
        let current_wire: Vec<u8> = current.get(2);
        if current_revision == vault.revision && current_wire == signed_wire_record {
            transaction.commit().await.map_err(ApiError::database)?;
            return vault_response(StatusCode::OK, vault.revision);
        }
        if current_revision != expected_revision {
            return Err(ApiError::precondition_failed("vault_revision_conflict"));
        }
        if current_deleted {
            return Err(ApiError::gone("account_deleted"));
        }
    } else if expected_revision != 0 {
        return Err(ApiError::precondition_failed("vault_revision_conflict"));
    }

    transaction
        .execute(
            "INSERT INTO noise.account_vault_versions (
                locator, revision, nonce, ciphertext, deleted, signature,
                signed_wire_record
             ) VALUES ($1, $2::text::numeric, $3, $4, $5, $6, $7)",
            &[
                &locator_bytes.as_slice(),
                &revision_text,
                &nonce,
                &ciphertext,
                &vault.deleted,
                &signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(vault_conflict)?;
    transaction
        .execute(
            "INSERT INTO noise.account_vault_heads (locator, revision)
             VALUES ($1, $2::text::numeric)
             ON CONFLICT (locator) DO UPDATE
             SET revision = EXCLUDED.revision, updated_at = clock_timestamp()",
            &[&locator_bytes.as_slice(), &revision_text],
        )
        .await
        .map_err(ApiError::database)?;

    if vault.deleted {
        transaction
            .execute(
                "UPDATE noise.accounts
                 SET status = 'deleted', deleted_at = clock_timestamp()
                 WHERE account_id = $1",
                &[&session.account_id],
            )
            .await
            .map_err(ApiError::database)?;
        transaction
            .execute(
                "UPDATE noise.sessions
                 SET revoked_at = clock_timestamp()
                 WHERE account_id = $1 AND revoked_at IS NULL",
                &[&session.account_id],
            )
            .await
            .map_err(ApiError::database)?;
    }
    transaction.commit().await.map_err(ApiError::database)?;
    vault_response(
        if is_new_locator {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        vault.revision,
    )
}

fn if_match(headers: &HeaderMap) -> Result<u64, ApiError> {
    let value = headers
        .get(IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::precondition_required)?;
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    value
        .parse()
        .map_err(|_| ApiError::bad_request("invalid_if_match"))
}

fn etag(revision: u64) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!("\"{revision}\"")).map_err(ApiError::database)
}

fn vault_response(status: StatusCode, revision: u64) -> Result<Response, ApiError> {
    let mut headers = HeaderMap::new();
    headers.insert(ETAG, etag(revision)?);
    Ok((status, headers).into_response())
}

fn decode_canonical_vec(encoded: &str, error_code: &'static str) -> Result<Vec<u8>, ApiError> {
    let decoded = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| ApiError::bad_request(error_code))?;
    if STANDARD_NO_PAD.encode(&decoded) != encoded
        || decoded.len() <= 16
        || decoded.len() > 2_000_016
    {
        return Err(ApiError::bad_request(error_code));
    }
    Ok(decoded)
}

fn vault_conflict(error: tokio_postgres::Error) -> ApiError {
    if error.as_db_error().is_some_and(|database| {
        database.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
    }) {
        ApiError::conflict("vault_conflict")
    } else {
        ApiError::database(error)
    }
}
