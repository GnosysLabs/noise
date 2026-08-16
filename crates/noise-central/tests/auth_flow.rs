use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::Parser;
use noise_central::{CentralConfig, build_app};
use noise_core::{
    AccountVault, CentralInstallationAuthKey, DirectMessagePolicy, GroupEventPayload,
    GroupMembership, Identity, MlsAccountState, MlsControlLog, MlsJoinRequest, MlsRemovalRequest,
    Profile, SignedEvent, derive_account_credentials,
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
struct DirectEventAcceptance {
    event_id: String,
    direct_scope_id: String,
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
    verify_mls_control_flow(
        &http,
        &base,
        &database,
        &identity,
        &first_session.access_token,
    )
    .await;
    verify_direct_event_flow(
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

    let watches = database
        .query_one(
            "SELECT count(*) FROM noise.watch_changes
             WHERE scope_id = $1 AND control = false",
            &[&group.group_id],
        )
        .await
        .unwrap();
    assert_eq!(watches.get::<_, i64>(0), 2);
}

async fn verify_mls_control_flow(
    http: &reqwest::Client,
    base: &str,
    database: &tokio_postgres::Client,
    founder: &Identity,
    founder_access_token: &str,
) {
    let joiner = Identity::from_secret_base64(&STANDARD_NO_PAD.encode([0x55; 32])).unwrap();
    let joiner_installation_key =
        CentralInstallationAuthKey::from_secret_base64(&STANDARD_NO_PAD.encode([0x66; 32]))
            .unwrap();
    let joiner_installation_id = STANDARD_NO_PAD.encode([0x77; 32]);
    let joiner_session = register_test_installation(
        http,
        base,
        &joiner,
        &joiner_installation_key,
        &joiner_installation_id,
    )
    .await;

    let group =
        GroupMembership::create_owned("MLS central integration", founder.public_key_base64());
    let mut founder_mls = MlsAccountState::create(founder).unwrap();
    let mut joiner_mls = MlsAccountState::create(&joiner).unwrap();
    let genesis = founder_mls.create_group_genesis(founder, &group).unwrap();

    assert_status(
        http.post(format!("{base}/v2/mls/genesis"))
            .bearer_auth(founder_access_token)
            .json(&genesis)
            .send()
            .await
            .unwrap(),
        StatusCode::ACCEPTED,
    );
    assert_status(
        http.post(format!("{base}/v2/mls/genesis"))
            .bearer_auth(founder_access_token)
            .json(&genesis)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    );

    let initial_log: MlsControlLog = http
        .get(format!("{base}/v2/mls/groups/{}", group.group_id))
        .bearer_auth(&joiner_session.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    initial_log.verify().unwrap();
    assert_eq!(initial_log.genesis, genesis);
    assert!(initial_log.epochs.is_empty());

    let join_request =
        MlsJoinRequest::create(&joiner, &mut joiner_mls, group.group_id.clone()).unwrap();
    assert_status(
        http.post(format!("{base}/v2/mls/join-requests"))
            .bearer_auth(&joiner_session.access_token)
            .json(&join_request)
            .send()
            .await
            .unwrap(),
        StatusCode::ACCEPTED,
    );
    assert_status(
        http.post(format!("{base}/v2/mls/join-requests"))
            .bearer_auth(&joiner_session.access_token)
            .json(&join_request)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    );

    let pending_joins: Vec<MlsJoinRequest> = http
        .get(format!(
            "{base}/v2/mls/groups/{}/join-requests",
            group.group_id
        ))
        .bearer_auth(founder_access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending_joins, vec![join_request.clone()]);

    let admission = founder_mls
        .add_member(&group.group_id, &join_request.key_package_base64)
        .unwrap();
    let welcome = admission.welcome_base64.clone().unwrap();
    let first_epoch = founder_mls
        .create_epoch_record(founder, genesis.record_id.clone(), admission)
        .unwrap();
    assert_status(
        http.post(format!("{base}/v2/mls/epochs"))
            .bearer_auth(founder_access_token)
            .json(&first_epoch)
            .send()
            .await
            .unwrap(),
        StatusCode::ACCEPTED,
    );
    assert_status(
        http.post(format!("{base}/v2/mls/epochs"))
            .bearer_auth(founder_access_token)
            .json(&first_epoch)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    );

    let joiner_epoch = joiner_mls.join_group(&group.group_id, &welcome).unwrap();
    assert_eq!(joiner_epoch.epoch, 1);
    let joiner_event = SignedEvent::create_for_epoch(
        &joiner,
        group.group_id.clone(),
        &joiner_epoch.archive_key_base64,
        joiner_epoch.epoch,
        text_payload("joined through canonical MLS"),
        1,
    )
    .unwrap();
    let accepted = publish_event(
        http,
        base,
        &joiner_session.access_token,
        &joiner_event,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(accepted.canonical_cursor, 3);

    let removal_request = MlsRemovalRequest::self_left(&joiner, group.group_id.clone()).unwrap();
    assert_status(
        http.post(format!("{base}/v2/mls/removal-requests"))
            .bearer_auth(&joiner_session.access_token)
            .json(&removal_request)
            .send()
            .await
            .unwrap(),
        StatusCode::ACCEPTED,
    );
    let pending_removals: Vec<MlsRemovalRequest> = http
        .get(format!(
            "{base}/v2/mls/groups/{}/removal-requests",
            group.group_id
        ))
        .bearer_auth(founder_access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending_removals, vec![removal_request.clone()]);

    let removal = founder_mls
        .remove_member(&group.group_id, &joiner.public_key_base64())
        .unwrap();
    let second_epoch = founder_mls
        .create_epoch_record(founder, first_epoch.record_id.clone(), removal)
        .unwrap();
    assert_status(
        http.post(format!("{base}/v2/mls/epochs"))
            .bearer_auth(founder_access_token)
            .json(&second_epoch)
            .send()
            .await
            .unwrap(),
        StatusCode::ACCEPTED,
    );

    let retry_after_removal = http
        .post(format!("{base}/v2/mls/removal-requests"))
        .bearer_auth(&joiner_session.access_token)
        .json(&removal_request)
        .send()
        .await
        .unwrap();
    assert_eq!(retry_after_removal.status(), StatusCode::OK);

    let final_log: MlsControlLog = http
        .get(format!("{base}/v2/mls/groups/{}", group.group_id))
        .bearer_auth(&joiner_session.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    final_log.verify().unwrap();
    assert_eq!(
        final_log.epochs,
        vec![first_epoch.clone(), second_epoch.clone()]
    );

    let rejected_event = SignedEvent::create_for_epoch(
        &joiner,
        group.group_id.clone(),
        &joiner_epoch.archive_key_base64,
        joiner_epoch.epoch,
        text_payload("must not publish after removal"),
        2,
    )
    .unwrap();
    let rejected = http
        .post(format!("{base}/v1/events"))
        .bearer_auth(&joiner_session.access_token)
        .json(&rejected_event)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);

    let founder_epoch = founder_mls.epoch(&group.group_id).unwrap();
    let founder_event = SignedEvent::create_for_epoch(
        founder,
        group.group_id.clone(),
        &founder_epoch.archive_key_base64,
        founder_epoch.epoch,
        text_payload("founder remains after removal"),
        1,
    )
    .unwrap();
    let accepted = publish_event(
        http,
        base,
        founder_access_token,
        &founder_event,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(accepted.canonical_cursor, 4);

    let group_id = decode_hex(&group.group_id);
    let membership_counts = database
        .query_one(
            "SELECT
                count(*) FILTER (WHERE gm.active_until_cursor IS NULL),
                count(*) FILTER (WHERE gm.active_until_cursor IS NOT NULL)
             FROM noise.group_memberships gm
             JOIN noise.groups g ON g.group_pk = gm.group_pk
             WHERE g.protocol_group_id = $1",
            &[&group_id],
        )
        .await
        .unwrap();
    assert_eq!(membership_counts.get::<_, i64>(0), 1);
    assert_eq!(membership_counts.get::<_, i64>(1), 1);
    let epoch_members = database
        .query(
            "SELECT e.epoch::text, count(m.account_id)
             FROM noise.mls_epochs e
             JOIN noise.mls_epoch_members m ON m.epoch_record_id = e.record_id
             JOIN noise.groups g ON g.group_pk = e.group_pk
             WHERE g.protocol_group_id = $1
             GROUP BY e.epoch
             ORDER BY e.epoch",
            &[&group_id],
        )
        .await
        .unwrap();
    assert_eq!(epoch_members.len(), 2);
    assert_eq!(epoch_members[0].get::<_, &str>(0), "1");
    assert_eq!(epoch_members[0].get::<_, i64>(1), 2);
    assert_eq!(epoch_members[1].get::<_, &str>(0), "2");
    assert_eq!(epoch_members[1].get::<_, i64>(1), 1);

    let mls_watches = database
        .query_one(
            "SELECT count(*)
             FROM noise.watch_changes
             WHERE scope_id = $1 AND control = true",
            &[&group.group_id],
        )
        .await
        .unwrap();
    assert_eq!(mls_watches.get::<_, i64>(0), 5);
}

async fn verify_direct_event_flow(
    http: &reqwest::Client,
    base: &str,
    database: &tokio_postgres::Client,
    sender: &Identity,
    sender_access_token: &str,
) {
    let recipient = Identity::from_secret_base64(&STANDARD_NO_PAD.encode([0x88; 32])).unwrap();
    let recipient_installation_key =
        CentralInstallationAuthKey::from_secret_base64(&STANDARD_NO_PAD.encode([0x99; 32]))
            .unwrap();
    let recipient_installation_id = STANDARD_NO_PAD.encode([0xaa; 32]);
    let recipient_session = register_test_installation(
        http,
        base,
        &recipient,
        &recipient_installation_key,
        &recipient_installation_id,
    )
    .await;
    let sender_public_key = sender.public_key_base64();
    let recipient_public_key = recipient.public_key_base64();
    let sender_profile = Profile {
        username: "sender".into(),
        bio: String::new(),
        avatar: None,
        album: None,
        accepts_direct_messages: true,
        direct_message_policy: DirectMessagePolicy::Everyone,
    };
    let recipient_mailbox = sender
        .direct_mailbox(&recipient_public_key, &recipient_public_key)
        .unwrap();
    let first = SignedEvent::direct_message(
        sender,
        &recipient_mailbox,
        &recipient_public_key,
        &sender_profile,
        "canonical encrypted hello",
        None,
        None,
        1,
    )
    .unwrap();
    let first_acceptance = publish_direct_event(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        &first,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(first_acceptance.event_id, first.event_id);
    assert_eq!(
        first_acceptance.direct_scope_id,
        sender.direct_scope_id(&recipient_public_key).unwrap()
    );
    assert_eq!(first_acceptance.canonical_cursor, 5);

    let retry = publish_direct_event(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        &first,
        StatusCode::OK,
    )
    .await;
    assert_eq!(retry.canonical_cursor, first_acceptance.canonical_cursor);

    let sender_mailbox = sender
        .direct_mailbox(&recipient_public_key, &sender_public_key)
        .unwrap();
    let wrongly_addressed = SignedEvent::direct_message(
        sender,
        &sender_mailbox,
        &recipient_public_key,
        &sender_profile,
        "must not be accepted into the recipient thread",
        None,
        None,
        2,
    )
    .unwrap();
    let rejected = http
        .post(format!("{base}/v1/direct-events"))
        .bearer_auth(sender_access_token)
        .json(&serde_json::json!({
            "recipient_public_key": recipient_public_key,
            "event": wrongly_addressed,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let sender_page = fetch_direct_events(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        0,
        Some(1),
    )
    .await;
    assert_eq!(sender_page.events.len(), 1);
    assert_eq!(sender_page.events[0].event.event_id, first.event_id);
    assert_eq!(sender_page.next_cursor, 5);
    assert!(!sender_page.has_more);

    let recipient_page = fetch_direct_events(
        http,
        base,
        &recipient_session.access_token,
        &sender_public_key,
        0,
        None,
    )
    .await;
    assert_eq!(recipient_page.events.len(), 1);
    let recipient_view = recipient
        .direct_mailbox(&sender_public_key, &recipient_public_key)
        .unwrap();
    let GroupEventPayload::DirectMessage {
        recipient_public_key: decrypted_recipient,
        text,
        ..
    } = recipient_page.events[0]
        .event
        .decrypt(&recipient_view)
        .unwrap()
    else {
        panic!("expected a direct message");
    };
    assert_eq!(decrypted_recipient, recipient_public_key);
    assert_eq!(text, "canonical encrypted hello");
    let GroupEventPayload::DirectMessage {
        text: sender_view_text,
        ..
    } = sender_page.events[0]
        .event
        .decrypt(&recipient_mailbox)
        .unwrap()
    else {
        panic!("expected sender to decrypt the canonical receiver copy");
    };
    assert_eq!(sender_view_text, "canonical encrypted hello");

    let recipient_profile = Profile {
        username: "recipient".into(),
        ..sender_profile
    };
    let reply_mailbox = recipient
        .direct_mailbox(&sender_public_key, &sender_public_key)
        .unwrap();
    let reply = SignedEvent::direct_message(
        &recipient,
        &reply_mailbox,
        &sender_public_key,
        &recipient_profile,
        "canonical encrypted reply",
        None,
        None,
        1,
    )
    .unwrap();
    let reply_acceptance = publish_direct_event(
        http,
        base,
        &recipient_session.access_token,
        &sender_public_key,
        &reply,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(reply_acceptance.canonical_cursor, 6);
    assert_eq!(
        reply_acceptance.direct_scope_id,
        first_acceptance.direct_scope_id
    );

    let first_page = fetch_direct_events(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        0,
        Some(1),
    )
    .await;
    assert_eq!(first_page.events[0].event.event_id, first.event_id);
    assert_eq!(first_page.next_cursor, 5);
    assert!(first_page.has_more);
    let remaining = fetch_direct_events(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        first_page.next_cursor,
        None,
    )
    .await;
    assert_eq!(remaining.events.len(), 1);
    assert_eq!(remaining.events[0].event.event_id, reply.event_id);
    assert_eq!(remaining.next_cursor, 6);
    assert!(!remaining.has_more);

    let latest: CanonicalEventPage = http
        .get(format!("{base}/v1/direct-events/latest"))
        .bearer_auth(&recipient_session.access_token)
        .query(&[
            ("peer_public_key", sender_public_key.as_str()),
            ("limit", "1"),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(latest.events.len(), 1);
    assert_eq!(latest.events[0].event.event_id, reply.event_id);
    assert_eq!(latest.next_cursor, 6);
    assert!(latest.has_more);

    database
        .execute(
            "UPDATE noise.events
             SET hidden_at = clock_timestamp()
             WHERE event_id = $1",
            &[&decode_hex(&first.event_id)],
        )
        .await
        .unwrap();
    let moderated = fetch_direct_events(
        http,
        base,
        sender_access_token,
        &recipient_public_key,
        0,
        None,
    )
    .await;
    assert_eq!(moderated.events.len(), 1);
    assert_eq!(moderated.events[0].event.event_id, reply.event_id);
    assert_eq!(moderated.next_cursor, 6);

    let concurrent = SignedEvent::direct_message(
        sender,
        &recipient_mailbox,
        &recipient_public_key,
        &Profile {
            username: "sender".into(),
            bio: String::new(),
            avatar: None,
            album: None,
            accepts_direct_messages: true,
            direct_message_policy: DirectMessagePolicy::Everyone,
        },
        "one event despite a concurrent retry",
        None,
        None,
        3,
    )
    .unwrap();
    let concurrent_body = serde_json::json!({
        "recipient_public_key": recipient_public_key,
        "event": concurrent,
    });
    let first_request = http
        .post(format!("{base}/v1/direct-events"))
        .bearer_auth(sender_access_token)
        .json(&concurrent_body)
        .send();
    let second_request = http
        .post(format!("{base}/v1/direct-events"))
        .bearer_auth(sender_access_token)
        .json(&concurrent_body)
        .send();
    let (first_response, second_response) = tokio::join!(first_request, second_request);
    let first_response = first_response.unwrap();
    let second_response = second_response.unwrap();
    assert_eq!(
        [first_response.status(), second_response.status()]
            .into_iter()
            .filter(|status| *status == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        [first_response.status(), second_response.status()]
            .into_iter()
            .filter(|status| *status == StatusCode::OK)
            .count(),
        1
    );
    let first_concurrent: DirectEventAcceptance = first_response.json().await.unwrap();
    let second_concurrent: DirectEventAcceptance = second_response.json().await.unwrap();
    assert_eq!(first_concurrent.canonical_cursor, 7);
    assert_eq!(
        first_concurrent.canonical_cursor,
        second_concurrent.canonical_cursor
    );

    let structure = database
        .query_one(
            "SELECT
                (SELECT count(*) FROM noise.direct_threads),
                (SELECT count(*) FROM noise.streams WHERE stream_kind = 'direct'),
                (SELECT count(*) FROM noise.events WHERE scope_kind = 'direct')",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(structure.get::<_, i64>(0), 1);
    assert_eq!(structure.get::<_, i64>(1), 1);
    assert_eq!(structure.get::<_, i64>(2), 3);
}

async fn register_test_installation(
    http: &reqwest::Client,
    base: &str,
    identity: &Identity,
    installation_key: &CentralInstallationAuthKey,
    installation_id: &str,
) -> SessionResponse {
    let registration_challenge = challenge(
        http,
        &format!("{base}/v1/auth/challenges/registration"),
        serde_json::json!({
            "account_public_key": identity.public_key_base64(),
        }),
    )
    .await;
    let registration = identity
        .central_installation_registration(
            installation_id,
            installation_key.public_key_base64(),
            &registration_challenge.challenge_id_base64,
            &registration_challenge.challenge_nonce_base64,
            now_millis(),
            1,
        )
        .unwrap();
    assert_status(
        http.post(format!("{base}/v1/devices/register"))
            .json(&registration)
            .send()
            .await
            .unwrap(),
        StatusCode::CREATED,
    );
    let session_challenge = session_challenge(http, base, identity, installation_id).await;
    let proof = installation_key
        .session_proof(
            identity.public_key_base64(),
            installation_id,
            &session_challenge.challenge_id_base64,
            &session_challenge.challenge_nonce_base64,
            now_millis(),
        )
        .unwrap();
    open_session(http, base, &proof).await
}

async fn publish_direct_event(
    http: &reqwest::Client,
    base: &str,
    access_token: &str,
    recipient_public_key: &str,
    event: &SignedEvent,
    expected_status: StatusCode,
) -> DirectEventAcceptance {
    let response = http
        .post(format!("{base}/v1/direct-events"))
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "recipient_public_key": recipient_public_key,
            "event": event,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), expected_status);
    response.json().await.unwrap()
}

async fn fetch_direct_events(
    http: &reqwest::Client,
    base: &str,
    access_token: &str,
    peer_public_key: &str,
    after: i64,
    limit: Option<u16>,
) -> CanonicalEventPage {
    let mut request = http
        .get(format!("{base}/v1/direct-events"))
        .bearer_auth(access_token)
        .query(&[
            ("peer_public_key", peer_public_key),
            ("after", &after.to_string()),
        ]);
    if let Some(limit) = limit {
        request = request.query(&[("limit", limit)]);
    }
    request
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn text_payload(text: &str) -> GroupEventPayload {
    GroupEventPayload::Message {
        text: text.to_owned(),
        attachment: None,
        reply_to_message_id: None,
        forwarded_from: None,
    }
}

fn assert_status(response: reqwest::Response, expected: StatusCode) {
    assert_eq!(response.status(), expected);
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
