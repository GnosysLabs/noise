mod config;
mod database;
mod error;
mod events;
mod media;
mod mls;
mod vaults;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE, ETAG, IF_MATCH},
    },
    response::IntoResponse,
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use ed25519_dalek::VerifyingKey;
use noise_core::{
    CentralInstallationRegistrationV1, CentralInstallationRevocationV1, CentralSessionProofV1,
};
use rand::random;
use serde::{Deserialize, Serialize};
use tokio_postgres::error::SqlState;
use tower_http::cors::CorsLayer;

pub use config::CentralConfig;
use database::Database;
use error::ApiError;

const CHALLENGE_LIFETIME_SECONDS: i64 = 120;
const SESSION_LIFETIME_SECONDS: i64 = 60 * 60;
const MAX_ACTIVE_CHALLENGES: i64 = 5;
const MAX_BODY_BYTES: usize = 3_000_000;

#[derive(Clone)]
struct AppState {
    database: Database,
    token_hash_key: [u8; 32],
    legacy_media: Option<media::LegacyMediaStore>,
}

struct AuthenticatedSession {
    account_id: i64,
    identity_public_key: [u8; 32],
}

#[derive(Deserialize)]
struct RegistrationChallengeRequest {
    account_public_key: String,
}

#[derive(Deserialize)]
struct SessionChallengeRequest {
    account_public_key: String,
    installation_id_base64: String,
}

#[derive(Serialize)]
struct ChallengeResponse {
    challenge_id_base64: String,
    challenge_nonce_base64: String,
    expires_at_millis: i64,
}

#[derive(Serialize)]
struct InstallationResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct SessionResponse {
    access_token: String,
    token_type: &'static str,
    expires_at_millis: i64,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    schema_version: i32,
}

pub async fn build_app(config: &CentralConfig) -> anyhow::Result<Router> {
    config.validate()?;
    let legacy_media = media::LegacyMediaStore::open(config)?;
    let state = Arc::new(AppState {
        database: Database::connect(config).await?,
        token_hash_key: config.token_hash_key()?,
        legacy_media,
    });

    let mut app = Router::new()
        .route("/health", get(health))
        .route(
            "/v1/auth/challenges/registration",
            post(issue_registration_challenge),
        )
        .route("/v1/auth/challenges/session", post(issue_session_challenge))
        .route("/v1/devices/register", post(register_installation))
        .route("/v1/auth/sessions", post(open_session))
        .route("/v1/auth/sessions/current", delete(close_current_session))
        .route(
            "/v1/account-vaults/{locator}",
            get(vaults::get_account_vault).put(vaults::put_account_vault),
        )
        .route("/v1/events", post(events::publish_group_event))
        .route(
            "/v1/direct-events",
            get(events::direct_events_after).post(events::publish_direct_event),
        )
        .route(
            "/v1/direct-events/latest",
            get(events::latest_direct_events),
        )
        .route(
            "/v1/groups/{group_id}/events",
            get(events::group_events_after),
        )
        .route(
            "/v1/groups/{group_id}/events/latest",
            get(events::latest_group_events),
        )
        .route("/v2/mls/genesis", post(mls::publish_genesis))
        .route("/v2/mls/epochs", post(mls::publish_epoch))
        .route("/v2/mls/join-requests", post(mls::publish_join_request))
        .route(
            "/v2/mls/removal-requests",
            post(mls::publish_removal_request),
        )
        .route("/v2/mls/groups/{group_id}", get(mls::control_log))
        .route(
            "/internal/compat/v1/providers/{provider}/shards/{shard_id}",
            get(media::get_legacy_shard),
        )
        .route(
            "/v2/mls/groups/{group_id}/join-requests",
            get(mls::join_requests),
        )
        .route(
            "/v2/mls/groups/{group_id}/removal-requests",
            get(mls::removal_requests),
        )
        .route(
            "/v1/devices/{installation_id}/revoke",
            post(revoke_installation),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state);

    if let Some(origin) = config.allowed_origin.as_deref() {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(HeaderValue::from_str(origin)?)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers([CONTENT_TYPE, AUTHORIZATION, IF_MATCH])
                .expose_headers([ETAG]),
        );
    }

    Ok(app)
}

async fn health(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, ApiError> {
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_one(
            "SELECT version FROM noise.schema_migrations ORDER BY version DESC LIMIT 1",
            &[],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(Json(HealthResponse {
        status: "ok",
        schema_version: row.get(0),
    }))
}

async fn issue_registration_challenge(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegistrationChallengeRequest>,
) -> Result<Json<ChallengeResponse>, ApiError> {
    let account_public_key = decode_identity_public_key(&request.account_public_key)?;
    let challenge_id = random::<[u8; 32]>();
    let challenge_nonce = random::<[u8; 32]>();
    let nonce_hash = blake3::hash(&challenge_nonce);

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.accounts (identity_public_key)
             VALUES ($1)
             ON CONFLICT (identity_public_key) DO NOTHING",
            &[&account_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    let account = transaction
        .query_one(
            "SELECT account_id, status
             FROM noise.accounts
             WHERE identity_public_key = $1
             FOR UPDATE",
            &[&account_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    let account_id: i64 = account.get(0);
    let status: &str = account.get(1);
    if status != "active" {
        return Err(ApiError::forbidden());
    }
    expire_challenges(&transaction, account_id, None, "register_device").await?;
    enforce_challenge_limit(&transaction, account_id, None, "register_device").await?;
    let expires = transaction
        .query_one(
            "INSERT INTO noise.auth_challenges (
                challenge_id, purpose, account_id, nonce_hash, expires_at
             ) VALUES (
                $1, 'register_device', $2, $3,
                clock_timestamp() + ($4::bigint * interval '1 second')
             )
             RETURNING (extract(epoch FROM expires_at) * 1000)::bigint",
            &[
                &challenge_id.as_slice(),
                &account_id,
                &nonce_hash.as_bytes().as_slice(),
                &CHALLENGE_LIFETIME_SECONDS,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;

    Ok(Json(ChallengeResponse {
        challenge_id_base64: STANDARD_NO_PAD.encode(challenge_id),
        challenge_nonce_base64: STANDARD_NO_PAD.encode(challenge_nonce),
        expires_at_millis: expires.get(0),
    }))
}

async fn issue_session_challenge(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionChallengeRequest>,
) -> Result<Json<ChallengeResponse>, ApiError> {
    let account_public_key = decode_identity_public_key(&request.account_public_key)?;
    let installation_id =
        decode_canonical_array::<32>(&request.installation_id_base64, "invalid_installation_id")?;
    let challenge_id = random::<[u8; 32]>();
    let challenge_nonce = random::<[u8; 32]>();
    let nonce_hash = blake3::hash(&challenge_nonce);

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let device = transaction
        .query_opt(
            "SELECT a.account_id, d.device_pk
             FROM noise.accounts a
             JOIN noise.devices d ON d.account_id = a.account_id
             WHERE a.identity_public_key = $1
               AND a.status = 'active'
               AND d.installation_id = $2
               AND d.revoked_at IS NULL
             FOR UPDATE OF d",
            &[&account_public_key.as_slice(), &installation_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let account_id: i64 = device.get(0);
    let device_pk: i64 = device.get(1);
    expire_challenges(&transaction, account_id, Some(device_pk), "open_session").await?;
    enforce_challenge_limit(&transaction, account_id, Some(device_pk), "open_session").await?;
    let expires = transaction
        .query_one(
            "INSERT INTO noise.auth_challenges (
                challenge_id, purpose, account_id, device_pk, nonce_hash, expires_at
             ) VALUES (
                $1, 'open_session', $2, $3, $4,
                clock_timestamp() + ($5::bigint * interval '1 second')
             )
             RETURNING (extract(epoch FROM expires_at) * 1000)::bigint",
            &[
                &challenge_id.as_slice(),
                &account_id,
                &device_pk,
                &nonce_hash.as_bytes().as_slice(),
                &CHALLENGE_LIFETIME_SECONDS,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;

    Ok(Json(ChallengeResponse {
        challenge_id_base64: STANDARD_NO_PAD.encode(challenge_id),
        challenge_nonce_base64: STANDARD_NO_PAD.encode(challenge_nonce),
        expires_at_millis: expires.get(0),
    }))
}

async fn register_installation(
    State(state): State<Arc<AppState>>,
    Json(registration): Json<CentralInstallationRegistrationV1>,
) -> Result<impl IntoResponse, ApiError> {
    registration
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_registration"))?;

    let account_public_key =
        decode_canonical_array::<32>(&registration.account_public_key, "invalid_account_key")?;
    let installation_id = decode_canonical_array::<32>(
        &registration.installation_id_base64,
        "invalid_installation_id",
    )?;
    let auth_public_key = decode_canonical_array::<32>(
        &registration.installation_auth_public_key_base64,
        "invalid_installation_auth_key",
    )?;
    let challenge_id =
        decode_canonical_array::<32>(&registration.challenge_id_base64, "invalid_challenge")?;
    let challenge_nonce =
        decode_canonical_array::<32>(&registration.challenge_nonce_base64, "invalid_challenge")?;
    let signature =
        decode_canonical_array::<64>(&registration.signature_base64, "invalid_registration")?;
    let nonce_hash = blake3::hash(&challenge_nonce);
    let registration_version = registration.registration_version.to_string();
    let registration_issued_at_millis = registration.issued_at_millis.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let account = transaction
        .query_opt(
            "SELECT account_id, status
             FROM noise.accounts
             WHERE identity_public_key = $1
             FOR UPDATE",
            &[&account_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let account_id: i64 = account.get(0);
    let account_status: &str = account.get(1);
    if account_status != "active" {
        return Err(ApiError::forbidden());
    }

    if let Some(existing) = transaction
        .query_opt(
            "SELECT
                device_pk,
                auth_public_key,
                registration_version::text,
                registration_challenge_id,
                registration_issued_at_millis::text,
                registration_signature,
                revoked_at IS NOT NULL
             FROM noise.devices
             WHERE account_id = $1 AND installation_id = $2
             FOR UPDATE",
            &[&account_id, &installation_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        let device_pk: i64 = existing.get(0);
        let existing_auth_public_key: Vec<u8> = existing.get(1);
        let existing_version = parse_u64(existing.get::<_, &str>(2))?;
        let existing_challenge_id: Vec<u8> = existing.get(3);
        let existing_issued_at = parse_u64(existing.get::<_, &str>(4))?;
        let existing_signature: Vec<u8> = existing.get(5);
        let revoked: bool = existing.get(6);
        if revoked {
            return Err(ApiError::conflict("installation_revoked"));
        }
        if existing_version == registration.registration_version
            && existing_auth_public_key == auth_public_key
            && existing_challenge_id == challenge_id
            && existing_issued_at == registration.issued_at_millis
            && existing_signature == signature
        {
            transaction.commit().await.map_err(ApiError::database)?;
            return Ok((
                StatusCode::OK,
                Json(InstallationResponse {
                    status: "registered",
                }),
            ));
        }
        if registration.registration_version <= existing_version {
            return Err(ApiError::conflict("installation_conflict"));
        }
        reject_auth_key_reuse(&transaction, &auth_public_key, Some(device_pk)).await?;
        consume_challenge(
            &transaction,
            &challenge_id,
            account_id,
            None,
            "register_device",
            nonce_hash.as_bytes(),
        )
        .await?;
        transaction
            .execute(
                "UPDATE noise.sessions
                 SET revoked_at = clock_timestamp()
                 WHERE device_pk = $1 AND revoked_at IS NULL",
                &[&device_pk],
            )
            .await
            .map_err(ApiError::database)?;
        transaction
            .execute(
                "UPDATE noise.devices
                 SET auth_public_key = $2,
                     registration_version = $3::text::numeric,
                     registration_challenge_id = $4,
                     registration_issued_at_millis = $5::text::numeric,
                     registration_signature = $6,
                     last_seen_at = clock_timestamp()
                 WHERE device_pk = $1",
                &[
                    &device_pk,
                    &auth_public_key.as_slice(),
                    &registration_version,
                    &challenge_id.as_slice(),
                    &registration_issued_at_millis,
                    &signature.as_slice(),
                ],
            )
            .await
            .map_err(registration_database_error)?;
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok((
            StatusCode::OK,
            Json(InstallationResponse {
                status: "registered",
            }),
        ));
    }

    reject_auth_key_reuse(&transaction, &auth_public_key, None).await?;
    consume_challenge(
        &transaction,
        &challenge_id,
        account_id,
        None,
        "register_device",
        nonce_hash.as_bytes(),
    )
    .await?;
    transaction
        .execute(
            "INSERT INTO noise.devices (
                account_id,
                installation_id,
                auth_public_key,
                registration_version,
                registration_challenge_id,
                registration_issued_at_millis,
                registration_signature
             ) VALUES ($1, $2, $3, $4::text::numeric, $5, $6::text::numeric, $7)",
            &[
                &account_id,
                &installation_id.as_slice(),
                &auth_public_key.as_slice(),
                &registration_version,
                &challenge_id.as_slice(),
                &registration_issued_at_millis,
                &signature.as_slice(),
            ],
        )
        .await
        .map_err(registration_database_error)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok((
        StatusCode::CREATED,
        Json(InstallationResponse {
            status: "registered",
        }),
    ))
}

async fn open_session(
    State(state): State<Arc<AppState>>,
    Json(proof): Json<CentralSessionProofV1>,
) -> Result<Json<SessionResponse>, ApiError> {
    proof
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_session_proof"))?;

    let account_public_key =
        decode_canonical_array::<32>(&proof.account_public_key, "invalid_account_key")?;
    let installation_id =
        decode_canonical_array::<32>(&proof.installation_id_base64, "invalid_installation_id")?;
    let auth_public_key = decode_canonical_array::<32>(
        &proof.installation_auth_public_key_base64,
        "invalid_installation_auth_key",
    )?;
    let challenge_id =
        decode_canonical_array::<32>(&proof.challenge_id_base64, "invalid_challenge")?;
    let challenge_nonce =
        decode_canonical_array::<32>(&proof.challenge_nonce_base64, "invalid_challenge")?;
    let nonce_hash = blake3::hash(&challenge_nonce);
    let session_id = random::<[u8; 32]>();
    let access_token = random::<[u8; 32]>();
    let token_hash = blake3::keyed_hash(&state.token_hash_key, &access_token);

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let device = transaction
        .query_opt(
            "SELECT a.account_id, d.device_pk
             FROM noise.accounts a
             JOIN noise.devices d ON d.account_id = a.account_id
             WHERE a.identity_public_key = $1
               AND a.status = 'active'
               AND d.installation_id = $2
               AND d.auth_public_key = $3
               AND d.revoked_at IS NULL
             FOR UPDATE OF d",
            &[
                &account_public_key.as_slice(),
                &installation_id.as_slice(),
                &auth_public_key.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let account_id: i64 = device.get(0);
    let device_pk: i64 = device.get(1);
    consume_challenge(
        &transaction,
        &challenge_id,
        account_id,
        Some(device_pk),
        "open_session",
        nonce_hash.as_bytes(),
    )
    .await?;
    let expires = transaction
        .query_one(
            "INSERT INTO noise.sessions (
                session_id,
                token_hash,
                account_id,
                device_pk,
                issued_from_challenge_id,
                expires_at
             ) VALUES (
                $1, $2, $3, $4, $5,
                clock_timestamp() + ($6::bigint * interval '1 second')
             )
             RETURNING (extract(epoch FROM expires_at) * 1000)::bigint",
            &[
                &session_id.as_slice(),
                &token_hash.as_bytes().as_slice(),
                &account_id,
                &device_pk,
                &challenge_id.as_slice(),
                &SESSION_LIFETIME_SECONDS,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction
        .execute(
            "UPDATE noise.devices
             SET last_seen_at = clock_timestamp()
             WHERE device_pk = $1",
            &[&device_pk],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;

    Ok(Json(SessionResponse {
        access_token: STANDARD_NO_PAD.encode(access_token),
        token_type: "Bearer",
        expires_at_millis: expires.get(0),
    }))
}

async fn close_current_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let token = bearer_token(&headers)?;
    let token_hash = blake3::keyed_hash(&state.token_hash_key, &token);
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let revoked = client
        .execute(
            "UPDATE noise.sessions s
             SET revoked_at = clock_timestamp()
             FROM noise.accounts a, noise.devices d
             WHERE s.token_hash = $1
               AND s.account_id = a.account_id
               AND s.device_pk = d.device_pk
               AND a.status = 'active'
               AND d.revoked_at IS NULL
               AND s.revoked_at IS NULL
               AND s.expires_at > clock_timestamp()",
            &[&token_hash.as_bytes().as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    if revoked != 1 {
        return Err(ApiError::unauthorized());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_installation(
    State(state): State<Arc<AppState>>,
    Path(path_installation_id): Path<String>,
    Json(revocation): Json<CentralInstallationRevocationV1>,
) -> Result<Json<InstallationResponse>, ApiError> {
    revocation
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_revocation"))?;
    if path_installation_id != revocation.installation_id_base64 {
        return Err(ApiError::bad_request("installation_mismatch"));
    }

    let account_public_key =
        decode_canonical_array::<32>(&revocation.account_public_key, "invalid_account_key")?;
    let installation_id = decode_canonical_array::<32>(
        &revocation.installation_id_base64,
        "invalid_installation_id",
    )?;
    let auth_public_key = decode_canonical_array::<32>(
        &revocation.installation_auth_public_key_base64,
        "invalid_installation_auth_key",
    )?;
    let signature =
        decode_canonical_array::<64>(&revocation.signature_base64, "invalid_revocation")?;
    let revocation_sequence = revocation.revocation_sequence.to_string();
    let revocation_issued_at_millis = revocation.issued_at_millis.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let device = transaction
        .query_opt(
            "SELECT
                d.device_pk,
                d.revocation_sequence::text,
                d.revocation_issued_at_millis::text,
                d.revocation_signature,
                d.revoked_at IS NOT NULL
             FROM noise.accounts a
             JOIN noise.devices d ON d.account_id = a.account_id
             WHERE a.identity_public_key = $1
               AND a.status <> 'deleted'
               AND d.installation_id = $2
               AND d.auth_public_key = $3
             FOR UPDATE OF d",
            &[
                &account_public_key.as_slice(),
                &installation_id.as_slice(),
                &auth_public_key.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let device_pk: i64 = device.get(0);
    let existing_sequence = parse_u64(device.get::<_, &str>(1))?;
    let existing_issued_at = device
        .get::<_, Option<&str>>(2)
        .map(parse_u64)
        .transpose()?;
    let existing_signature: Option<Vec<u8>> = device.get(3);
    let already_revoked: bool = device.get(4);
    if already_revoked
        && existing_sequence == revocation.revocation_sequence
        && existing_issued_at == Some(revocation.issued_at_millis)
        && existing_signature.as_deref() == Some(signature.as_slice())
    {
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok(Json(InstallationResponse { status: "revoked" }));
    }
    if revocation.revocation_sequence <= existing_sequence {
        return Err(ApiError::conflict("stale_revocation"));
    }
    transaction
        .execute(
            "UPDATE noise.devices
             SET revoked_at = COALESCE(revoked_at, clock_timestamp()),
                 revocation_sequence = $2::text::numeric,
                 revocation_issued_at_millis = $3::text::numeric,
                 revocation_signature = $4
             WHERE device_pk = $1",
            &[
                &device_pk,
                &revocation_sequence,
                &revocation_issued_at_millis,
                &signature.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction
        .execute(
            "UPDATE noise.sessions
             SET revoked_at = clock_timestamp()
             WHERE device_pk = $1 AND revoked_at IS NULL",
            &[&device_pk],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(Json(InstallationResponse { status: "revoked" }))
}

async fn expire_challenges(
    transaction: &tokio_postgres::Transaction<'_>,
    account_id: i64,
    device_pk: Option<i64>,
    purpose: &str,
) -> Result<(), ApiError> {
    transaction
        .execute(
            "UPDATE noise.auth_challenges
             SET consumed_at = clock_timestamp()
             WHERE account_id = $1
               AND device_pk IS NOT DISTINCT FROM $2
               AND purpose = $3
               AND consumed_at IS NULL
               AND expires_at <= clock_timestamp()",
            &[&account_id, &device_pk, &purpose],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(())
}

async fn enforce_challenge_limit(
    transaction: &tokio_postgres::Transaction<'_>,
    account_id: i64,
    device_pk: Option<i64>,
    purpose: &str,
) -> Result<(), ApiError> {
    let row = transaction
        .query_one(
            "SELECT count(*)
             FROM noise.auth_challenges
             WHERE account_id = $1
               AND device_pk IS NOT DISTINCT FROM $2
               AND purpose = $3
               AND consumed_at IS NULL
               AND expires_at > clock_timestamp()",
            &[&account_id, &device_pk, &purpose],
        )
        .await
        .map_err(ApiError::database)?;
    let count: i64 = row.get(0);
    if count >= MAX_ACTIVE_CHALLENGES {
        return Err(ApiError::too_many_requests());
    }
    Ok(())
}

async fn consume_challenge(
    transaction: &tokio_postgres::Transaction<'_>,
    challenge_id: &[u8; 32],
    account_id: i64,
    device_pk: Option<i64>,
    purpose: &str,
    nonce_hash: &[u8; 32],
) -> Result<(), ApiError> {
    let consumed = transaction
        .execute(
            "UPDATE noise.auth_challenges
             SET consumed_at = clock_timestamp()
             WHERE challenge_id = $1
               AND account_id = $2
               AND device_pk IS NOT DISTINCT FROM $3
               AND purpose = $4
               AND nonce_hash = $5
               AND consumed_at IS NULL
               AND expires_at > clock_timestamp()",
            &[
                &challenge_id.as_slice(),
                &account_id,
                &device_pk,
                &purpose,
                &nonce_hash.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?;
    if consumed != 1 {
        return Err(ApiError::conflict("challenge_unavailable"));
    }
    Ok(())
}

async fn reject_auth_key_reuse(
    transaction: &tokio_postgres::Transaction<'_>,
    auth_public_key: &[u8; 32],
    allowed_device_pk: Option<i64>,
) -> Result<(), ApiError> {
    let existing = transaction
        .query_opt(
            "SELECT device_pk
             FROM noise.devices
             WHERE auth_public_key = $1
             FOR UPDATE",
            &[&auth_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    if existing.is_some_and(|row| Some(row.get::<_, i64>(0)) != allowed_device_pk) {
        return Err(ApiError::conflict("installation_auth_key_in_use"));
    }
    Ok(())
}

fn decode_identity_public_key(encoded: &str) -> Result<[u8; 32], ApiError> {
    let bytes = decode_canonical_array::<32>(encoded, "invalid_account_key")?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| ApiError::bad_request("invalid_account_key"))?;
    Ok(bytes)
}

fn decode_canonical_array<const N: usize>(
    encoded: &str,
    error_code: &'static str,
) -> Result<[u8; N], ApiError> {
    let decoded = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| ApiError::bad_request(error_code))?;
    let bytes: [u8; N] = decoded
        .try_into()
        .map_err(|_| ApiError::bad_request(error_code))?;
    if STANDARD_NO_PAD.encode(bytes) != encoded {
        return Err(ApiError::bad_request(error_code));
    }
    Ok(bytes)
}

fn bearer_token(headers: &HeaderMap) -> Result<[u8; 32], ApiError> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::unauthorized)?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(ApiError::unauthorized)?;
    decode_canonical_array::<32>(token, "unauthorized").map_err(|_| ApiError::unauthorized())
}

async fn authenticate_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, ApiError> {
    let token = bearer_token(headers)?;
    let token_hash = blake3::keyed_hash(&state.token_hash_key, &token);
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_opt(
            "SELECT s.account_id, a.identity_public_key
             FROM noise.sessions s
             JOIN noise.accounts a ON a.account_id = s.account_id
             JOIN noise.devices d
               ON d.device_pk = s.device_pk AND d.account_id = s.account_id
             LEFT JOIN noise.account_restrictions ar
               ON ar.account_id = a.account_id
              AND (ar.expires_at IS NULL OR ar.expires_at > clock_timestamp())
             WHERE s.token_hash = $1
               AND s.revoked_at IS NULL
               AND s.expires_at > clock_timestamp()
               AND a.status = 'active'
               AND d.revoked_at IS NULL
               AND ar.account_id IS NULL",
            &[&token_hash.as_bytes().as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::unauthorized)?;
    let identity: Vec<u8> = row.get(1);
    let identity_public_key: [u8; 32] = identity
        .try_into()
        .map_err(|_| ApiError::database("stored identity key has an invalid length"))?;
    Ok(AuthenticatedSession {
        account_id: row.get(0),
        identity_public_key,
    })
}

fn decode_hex_32(value: &str, error_code: &'static str) -> Result<[u8; 32], ApiError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(error_code));
    }
    let mut decoded = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or_else(|| ApiError::bad_request(error_code))?;
        let low = hex_nibble(pair[1]).ok_or_else(|| ApiError::bad_request(error_code))?;
        decoded[index] = (high << 4) | low;
    }
    if encode_hex(&decoded) != value {
        return Err(ApiError::bad_request(error_code));
    }
    Ok(decoded)
}

fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn parse_u64(value: &str) -> Result<u64, ApiError> {
    value.parse().map_err(ApiError::database)
}

fn registration_database_error(error: tokio_postgres::Error) -> ApiError {
    if error
        .as_db_error()
        .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION)
    {
        ApiError::conflict("installation_conflict")
    } else {
        ApiError::database(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_tokens_require_exact_canonical_form() {
        let token = [0x42; 32];
        let encoded = STANDARD_NO_PAD.encode(token);
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {encoded}")).unwrap(),
        );
        assert_eq!(bearer_token(&headers).unwrap(), token);

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("bearer {encoded}")).unwrap(),
        );
        assert!(bearer_token(&headers).is_err());
    }
}
