use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::Parser;
use noise_central::{CentralConfig, build_app};
use noise_core::{CentralInstallationAuthKey, Identity};
use reqwest::StatusCode;
use serde::Deserialize;
use tokio::net::TcpListener;

#[derive(Deserialize)]
struct ChallengeResponse {
    challenge_id_base64: String,
    challenge_nonce_base64: String,
}

#[derive(Deserialize)]
struct SessionResponse {
    access_token: String,
}

#[tokio::test]
#[ignore = "requires an explicitly provisioned disposable PostgreSQL database"]
async fn complete_installation_session_and_revocation_flow() {
    let database_host = required_env("NOISE_CENTRAL_TEST_DATABASE_HOST");
    let database_port = required_env("NOISE_CENTRAL_TEST_DATABASE_PORT");
    let database_name = required_env("NOISE_CENTRAL_TEST_DATABASE_NAME");
    let database_user = required_env("NOISE_CENTRAL_TEST_DATABASE_USER");
    let database_password = required_env("NOISE_CENTRAL_TEST_DATABASE_PASSWORD");
    let token_hash_key = required_env("NOISE_CENTRAL_TEST_TOKEN_HASH_KEY");
    let config = CentralConfig::try_parse_from([
        "noise-central",
        "--database-host",
        &database_host,
        "--database-port",
        &database_port,
        "--database-name",
        &database_name,
        "--database-user",
        &database_user,
        "--database-password",
        &database_password,
        "--database-pool-size",
        "2",
        "--token-hash-key-base64",
        &token_hash_key,
    ])
    .unwrap();

    let app = build_app(&config).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{address}");
    let http = reqwest::Client::new();

    let health = http.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let identity = Identity::from_secret_base64(&STANDARD_NO_PAD.encode([0x11; 32])).unwrap();
    let installation_key =
        CentralInstallationAuthKey::from_secret_base64(&STANDARD_NO_PAD.encode([0x22; 32]))
            .unwrap();
    let installation_id = STANDARD_NO_PAD.encode([0x33; 32]);

    let registration_challenge = challenge(
        &http,
        &format!("{base}/v1/auth/challenges/registration"),
        serde_json::json!({
            "account_public_key": identity.public_key_base64(),
        }),
    )
    .await;
    let registration = identity
        .central_installation_registration(
            &installation_id,
            installation_key.public_key_base64(),
            &registration_challenge.challenge_id_base64,
            &registration_challenge.challenge_nonce_base64,
            now_millis(),
            1,
        )
        .unwrap();
    let registered = http
        .post(format!("{base}/v1/devices/register"))
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::CREATED);

    let idempotent_registration = http
        .post(format!("{base}/v1/devices/register"))
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(idempotent_registration.status(), StatusCode::OK);

    let first_session_challenge =
        session_challenge(&http, &base, &identity, &installation_id).await;
    let first_proof = installation_key
        .session_proof(
            identity.public_key_base64(),
            &installation_id,
            &first_session_challenge.challenge_id_base64,
            &first_session_challenge.challenge_nonce_base64,
            now_millis(),
        )
        .unwrap();
    let first_session = open_session(&http, &base, &first_proof).await;

    let reused_proof = http
        .post(format!("{base}/v1/auth/sessions"))
        .json(&first_proof)
        .send()
        .await
        .unwrap();
    assert_eq!(reused_proof.status(), StatusCode::CONFLICT);

    let logout = http
        .delete(format!("{base}/v1/auth/sessions/current"))
        .bearer_auth(&first_session.access_token)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    let second_session_challenge =
        session_challenge(&http, &base, &identity, &installation_id).await;
    let second_proof = installation_key
        .session_proof(
            identity.public_key_base64(),
            &installation_id,
            &second_session_challenge.challenge_id_base64,
            &second_session_challenge.challenge_nonce_base64,
            now_millis(),
        )
        .unwrap();
    let second_session = open_session(&http, &base, &second_proof).await;

    let revocation = identity
        .central_installation_revocation(
            &installation_id,
            installation_key.public_key_base64(),
            1,
            now_millis(),
        )
        .unwrap();
    let revoked = http
        .post(format!("{base}/v1/devices/{installation_id}/revoke"))
        .json(&revocation)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);

    let revoked_again = http
        .post(format!("{base}/v1/devices/{installation_id}/revoke"))
        .json(&revocation)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_again.status(), StatusCode::OK);

    let revoked_session = http
        .delete(format!("{base}/v1/auth/sessions/current"))
        .bearer_auth(&second_session.access_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_session.status(), StatusCode::UNAUTHORIZED);

    let challenge_after_revocation = http
        .post(format!("{base}/v1/auth/challenges/session"))
        .json(&serde_json::json!({
            "account_public_key": identity.public_key_base64(),
            "installation_id_base64": installation_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(challenge_after_revocation.status(), StatusCode::FORBIDDEN);

    let mut postgres = tokio_postgres::Config::new();
    postgres
        .host(&database_host)
        .port(database_port.parse().unwrap())
        .dbname(&database_name)
        .user(&database_user)
        .password(&database_password);
    let (database, connection) = postgres.connect(tokio_postgres::NoTls).await.unwrap();
    let connection_task = tokio::spawn(async move {
        connection.await.unwrap();
    });
    let row = database
        .query_one(
            "SELECT
                count(*) FILTER (WHERE revoked_at IS NOT NULL),
                bool_and(octet_length(token_hash) = 32)
             FROM noise.sessions",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 2);
    assert!(row.get::<_, bool>(1));
    let raw_second_token = STANDARD_NO_PAD.decode(second_session.access_token).unwrap();
    let raw_token_present = database
        .query_one(
            "SELECT EXISTS (
                SELECT 1 FROM noise.sessions WHERE token_hash = $1
             )",
            &[&raw_second_token],
        )
        .await
        .unwrap();
    assert!(!raw_token_present.get::<_, bool>(0));
    let raw_second_nonce = STANDARD_NO_PAD
        .decode(second_session_challenge.challenge_nonce_base64)
        .unwrap();
    let raw_nonce_present = database
        .query_one(
            "SELECT EXISTS (
                SELECT 1 FROM noise.auth_challenges WHERE nonce_hash = $1
             )",
            &[&raw_second_nonce],
        )
        .await
        .unwrap();
    assert!(!raw_nonce_present.get::<_, bool>(0));

    drop(database);
    connection_task.abort();
    server.abort();
}

async fn challenge(
    http: &reqwest::Client,
    url: &str,
    body: serde_json::Value,
) -> ChallengeResponse {
    let response = http.post(url).json(&body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn session_challenge(
    http: &reqwest::Client,
    base: &str,
    identity: &Identity,
    installation_id: &str,
) -> ChallengeResponse {
    challenge(
        http,
        &format!("{base}/v1/auth/challenges/session"),
        serde_json::json!({
            "account_public_key": identity.public_key_base64(),
            "installation_id_base64": installation_id,
        }),
    )
    .await
}

async fn open_session(
    http: &reqwest::Client,
    base: &str,
    proof: &noise_core::CentralSessionProofV1,
) -> SessionResponse {
    let response = http
        .post(format!("{base}/v1/auth/sessions"))
        .json(proof)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}
