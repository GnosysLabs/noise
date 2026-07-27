use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::Parser;
use noise_central::{CentralConfig, build_app};
use noise_core::{
    AccountVault, CentralInstallationAuthKey, GroupMembership, Identity, SignedEvent,
    derive_account_credentials,
};
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

#[derive(Deserialize)]
struct EventAcceptance {
    event_id: String,
    canonical_cursor: i64,
}

#[derive(Deserialize)]
struct CanonicalEvent {
    canonical_cursor: i64,
    event: SignedEvent,
}

#[derive(Deserialize)]
struct CanonicalEventPage {
    events: Vec<CanonicalEvent>,
    next_cursor: i64,
    has_more: bool,
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

    verify_account_vault_flow(&http, &base, &identity, &first_session.access_token).await;
    verify_group_event_flow(
        &http,
        &base,
        &database,
        &identity,
        &first_session.access_token,
    )
    .await;

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

async fn verify_account_vault_flow(
    http: &reqwest::Client,
    base: &str,
    identity: &Identity,
    access_token: &str,
) {
    let credentials =
        derive_account_credentials("1234 5678 9012", "integration test password").unwrap();
    let first =
        AccountVault::seal(identity, &credentials, 1, b"first encrypted account state").unwrap();
    let url = format!("{base}/v1/account-vaults/{}", credentials.locator);

    let missing_precondition = http
        .put(&url)
        .bearer_auth(access_token)
        .json(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing_precondition.status(),
        StatusCode::PRECONDITION_REQUIRED
    );

    let created = http
        .put(&url)
        .bearer_auth(access_token)
        .header("if-match", "\"0\"")
        .json(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers().get("etag").unwrap(), "\"1\"");

    let idempotent = http
        .put(&url)
        .bearer_auth(access_token)
        .header("if-match", "\"0\"")
        .json(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(idempotent.status(), StatusCode::OK);
    assert_eq!(idempotent.headers().get("etag").unwrap(), "\"1\"");

    let fetched = http.get(&url).send().await.unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.headers().get("etag").unwrap(), "\"1\"");
    assert_eq!(fetched.headers().get("cache-control").unwrap(), "no-store");
    let fetched_vault: AccountVault = fetched.json().await.unwrap();
    assert_eq!(
        fetched_vault.open(&credentials).unwrap(),
        b"first encrypted account state"
    );

    let second =
        AccountVault::seal(identity, &credentials, 2, b"second encrypted account state").unwrap();
    let stale = http
        .put(&url)
        .bearer_auth(access_token)
        .header("if-match", "\"0\"")
        .json(&second)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    let updated = http
        .put(&url)
        .bearer_auth(access_token)
        .header("if-match", "\"1\"")
        .json(&second)
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(updated.headers().get("etag").unwrap(), "\"2\"");
}

async fn verify_group_event_flow(
    http: &reqwest::Client,
    base: &str,
    database: &tokio_postgres::Client,
    identity: &Identity,
    access_token: &str,
) {
    let identity_key = STANDARD_NO_PAD
        .decode(identity.public_key_base64())
        .unwrap();
    let account = database
        .query_one(
            "SELECT account_id
             FROM noise.accounts
             WHERE identity_public_key = $1",
            &[&identity_key],
        )
        .await
        .unwrap();
    let account_id: i64 = account.get(0);
    let group = GroupMembership::create_owned("central integration", identity.public_key_base64());
    let group_id = decode_hex(&group.group_id);
    let group_row = database
        .query_one(
            "INSERT INTO noise.groups (protocol_group_id, founder_account_id)
             VALUES ($1, $2)
             RETURNING group_pk",
            &[&group_id, &account_id],
        )
        .await
        .unwrap();
    let group_pk: i64 = group_row.get(0);
    database
        .execute(
            "INSERT INTO noise.group_memberships (
                group_pk, account_id, role, source_kind, source_record_id,
                active_from_cursor
             ) VALUES ($1, $2, 'founder', 'legacy_import', $3, 0)",
            &[&group_pk, &account_id, &vec![0x44_u8; 32]],
        )
        .await
        .unwrap();

    let first = SignedEvent::chat(identity, &group, "first canonical event", 1).unwrap();
    let first_acceptance =
        publish_event(http, base, access_token, &first, StatusCode::CREATED).await;
    assert_eq!(first_acceptance.event_id, first.event_id);
    assert_eq!(first_acceptance.canonical_cursor, 1);

    let retry = publish_event(http, base, access_token, &first, StatusCode::OK).await;
    assert_eq!(retry.canonical_cursor, first_acceptance.canonical_cursor);

    let second = SignedEvent::chat(identity, &group, "second canonical event", 2).unwrap();
    let second_acceptance =
        publish_event(http, base, access_token, &second, StatusCode::CREATED).await;
    assert_eq!(second_acceptance.canonical_cursor, 2);

    let sequence_conflict =
        SignedEvent::chat(identity, &group, "conflicting second event", 2).unwrap();
    let rejected = http
        .post(format!("{base}/v1/events"))
        .bearer_auth(access_token)
        .json(&sequence_conflict)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);

    let first_page: CanonicalEventPage = http
        .get(format!(
            "{base}/v1/groups/{}/events?after=0&limit=1",
            group.group_id
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_page.events.len(), 1);
    assert_eq!(first_page.events[0].canonical_cursor, 1);
    assert_eq!(first_page.events[0].event.event_id, first.event_id);
    assert_eq!(first_page.next_cursor, 1);
    assert!(first_page.has_more);

    let remaining: CanonicalEventPage = http
        .get(format!(
            "{base}/v1/groups/{}/events?after=1",
            group.group_id
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(remaining.events.len(), 1);
    assert_eq!(remaining.events[0].event.event_id, second.event_id);
    assert_eq!(remaining.next_cursor, 2);
    assert!(!remaining.has_more);

    let latest: CanonicalEventPage = http
        .get(format!(
            "{base}/v1/groups/{}/events/latest?limit=1",
            group.group_id
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(latest.events.len(), 1);
    assert_eq!(latest.events[0].event.event_id, second.event_id);
    assert_eq!(latest.next_cursor, 2);
    assert!(latest.has_more);

    let outbox = database
        .query_one(
            "SELECT count(*) FROM noise.outbox_events
             WHERE topic = 'event.accepted'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(outbox.get::<_, i64>(0), 2);
}

async fn publish_event(
    http: &reqwest::Client,
    base: &str,
    access_token: &str,
    event: &SignedEvent,
    expected_status: StatusCode,
) -> EventAcceptance {
    let response = http
        .post(format!("{base}/v1/events"))
        .bearer_auth(access_token)
        .json(event)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), expected_status);
    response.json().await.unwrap()
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
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
