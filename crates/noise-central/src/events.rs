use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use deadpool_postgres::{GenericClient, Transaction};
use noise_core::{SignedEvent, direct_mailbox_id, direct_scope_id};
use serde::{Deserialize, Serialize};
use tokio_postgres::error::SqlState;

use super::{
    AppState, AuthenticatedSession, authenticate_session, decode_canonical_array, decode_hex_32,
    error::ApiError,
};

const DEFAULT_PAGE_SIZE: u16 = 100;
const MAX_PAGE_SIZE: u16 = 128;
const MAX_EVENT_CIPHERTEXT_BYTES: usize = 2_000_016;

#[derive(Deserialize)]
pub struct EventsAfterQuery {
    #[serde(default)]
    after: i64,
    limit: Option<u16>,
    stream_locator: Option<String>,
}

#[derive(Deserialize)]
pub struct LatestEventsQuery {
    limit: Option<u16>,
    stream_locator: Option<String>,
}

#[derive(Deserialize)]
pub struct DirectEventsAfterQuery {
    peer_public_key: String,
    #[serde(default)]
    after: i64,
    limit: Option<u16>,
}

#[derive(Deserialize)]
pub struct LatestDirectEventsQuery {
    peer_public_key: String,
    limit: Option<u16>,
}

#[derive(Deserialize)]
pub struct DirectEventPublication {
    recipient_public_key: String,
    event: SignedEvent,
}

#[derive(Serialize)]
pub struct EventAcceptance {
    status: &'static str,
    event_id: String,
    canonical_cursor: i64,
}

#[derive(Serialize)]
pub struct CanonicalEvent {
    canonical_cursor: i64,
    event: SignedEvent,
}

#[derive(Serialize)]
pub struct CanonicalEventPage {
    events: Vec<CanonicalEvent>,
    next_cursor: i64,
    has_more: bool,
}

#[derive(Serialize)]
pub struct DirectEventAcceptance {
    status: &'static str,
    event_id: String,
    direct_scope_id: String,
    canonical_cursor: i64,
}

pub async fn publish_group_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(event): Json<SignedEvent>,
) -> Result<(StatusCode, Json<EventAcceptance>), ApiError> {
    event
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_event"))?;
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedEvent::new(&event)?;
    if decoded.author_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("event_author_mismatch"));
    }
    let signed_wire_record = serde_json::to_vec(&event).map_err(ApiError::database)?;

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;

    if let Some(existing) = transaction
        .query_opt(
            "SELECT canonical_cursor, signed_wire_record
             FROM noise.events
             WHERE event_id = $1",
            &[&decoded.event_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        let existing_wire: Vec<u8> = existing.get(1);
        if existing_wire != signed_wire_record {
            return Err(ApiError::conflict("event_id_conflict"));
        }
        let cursor: i64 = existing.get(0);
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok((
            StatusCode::OK,
            Json(EventAcceptance {
                status: "accepted",
                event_id: event.event_id,
                canonical_cursor: cursor,
            }),
        ));
    }

    let group = authorize_group(&transaction, &session, &decoded.group_id, true).await?;
    validate_event_epoch(&transaction, &session, group.group_pk, &event).await?;
    let stream_pk = ensure_stream(
        &transaction,
        group.group_pk,
        decoded.stream_locator.as_ref(),
    )
    .await?;
    let cursor_row = transaction
        .query_one(
            "UPDATE noise.cursor_clock
             SET last_cursor = last_cursor + 1
             WHERE singleton
             RETURNING last_cursor",
            &[],
        )
        .await
        .map_err(ApiError::database)?;
    let canonical_cursor: i64 = cursor_row.get(0);
    let author_sequence = event.author_sequence.to_string();
    let created_at_millis = event.created_at_millis.to_string();
    let epoch = event.epoch.map(|value| value.to_string());
    transaction
        .execute(
            "INSERT INTO noise.events (
                event_id, canonical_cursor, scope_kind, protocol_scope_id,
                group_pk, stream_pk, author_account_id, author_sequence,
                created_at_millis, encryption_version, epoch,
                protocol_stream_locator, nonce, ciphertext, signature,
                signed_wire_record
             ) VALUES (
                $1, $2, 'group', $3, $4, $5, $6, $7::text::numeric,
                $8::text::numeric, $9, $10::text::numeric, $11, $12, $13,
                $14, $15
             )",
            &[
                &decoded.event_id.as_slice(),
                &canonical_cursor,
                &decoded.group_id.as_slice(),
                &group.group_pk,
                &stream_pk,
                &session.account_id,
                &author_sequence,
                &created_at_millis,
                &(event.encryption_version as i32),
                &epoch,
                &decoded
                    .stream_locator
                    .as_ref()
                    .map(|value| value.as_slice()),
                &decoded.nonce.as_slice(),
                &decoded.ciphertext,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(event_conflict)?;
    transaction
        .execute(
            "UPDATE noise.streams
             SET latest_cursor = $2
             WHERE stream_pk = $1",
            &[&stream_pk, &canonical_cursor],
        )
        .await
        .map_err(ApiError::database)?;
    let outbox_payload = serde_json::to_vec(&serde_json::json!({
        "canonical_cursor": canonical_cursor,
        "event_id": event.event_id,
    }))
    .map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.outbox_events (
                topic, aggregate_kind, aggregate_id, payload
             ) VALUES ('event.accepted', 'event', $1, $2)",
            &[&decoded.event_id.as_slice(), &outbox_payload],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;

    Ok((
        StatusCode::CREATED,
        Json(EventAcceptance {
            status: "accepted",
            event_id: event.event_id,
            canonical_cursor,
        }),
    ))
}

pub async fn group_events_after(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Query(query): Query<EventsAfterQuery>,
    headers: HeaderMap,
) -> Result<Json<CanonicalEventPage>, ApiError> {
    if query.after < 0 {
        return Err(ApiError::bad_request("invalid_cursor"));
    }
    let limit = page_limit(query.limit)?;
    let session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let stream_locator = query
        .stream_locator
        .as_deref()
        .map(|value| decode_hex_32(value, "invalid_stream_locator"))
        .transpose()?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group = authorize_group(&client, &session, &group_id, false).await?;
    let Some(stream) = find_stream(&client, group.group_pk, stream_locator.as_ref()).await? else {
        return Ok(Json(CanonicalEventPage {
            events: Vec::new(),
            next_cursor: query.after,
            has_more: false,
        }));
    };
    let mut events = read_events_after(
        &client,
        stream.stream_pk,
        query.after,
        stream.latest_cursor,
        limit + 1,
    )
    .await?;
    let has_more = events.len() > limit as usize;
    if has_more {
        events.pop();
    }
    let next_cursor = if has_more {
        events
            .last()
            .map_or(query.after, |event| event.canonical_cursor)
    } else {
        query.after.max(stream.latest_cursor)
    };
    Ok(Json(CanonicalEventPage {
        events,
        next_cursor,
        has_more,
    }))
}

pub async fn latest_group_events(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Query(query): Query<LatestEventsQuery>,
    headers: HeaderMap,
) -> Result<Json<CanonicalEventPage>, ApiError> {
    let limit = page_limit(query.limit)?;
    let session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let stream_locator = query
        .stream_locator
        .as_deref()
        .map(|value| decode_hex_32(value, "invalid_stream_locator"))
        .transpose()?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group = authorize_group(&client, &session, &group_id, false).await?;
    let Some(stream) = find_stream(&client, group.group_pk, stream_locator.as_ref()).await? else {
        return Ok(Json(CanonicalEventPage {
            events: Vec::new(),
            next_cursor: 0,
            has_more: false,
        }));
    };
    let mut events =
        read_latest_events(&client, stream.stream_pk, stream.latest_cursor, limit + 1).await?;
    let has_more = events.len() > limit as usize;
    if has_more {
        events.remove(0);
    }
    Ok(Json(CanonicalEventPage {
        events,
        next_cursor: stream.latest_cursor,
        has_more,
    }))
}

pub async fn publish_direct_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DirectEventPublication>,
) -> Result<(StatusCode, Json<DirectEventAcceptance>), ApiError> {
    request
        .event
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_event"))?;
    if request.event.encryption_version != 1
        || request.event.epoch.is_some()
        || request.event.stream_locator.is_some()
    {
        return Err(ApiError::bad_request("invalid_direct_event"));
    }
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedEvent::new(&request.event)?;
    if decoded.author_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("event_author_mismatch"));
    }
    let recipient_public_key =
        decode_canonical_array::<32>(&request.recipient_public_key, "invalid_recipient_key")?;
    if recipient_public_key == session.identity_public_key {
        return Err(ApiError::bad_request("invalid_direct_recipient"));
    }
    let expected_mailbox = direct_mailbox_id(&request.recipient_public_key)
        .map_err(|_| ApiError::bad_request("invalid_recipient_key"))
        .and_then(|value| decode_hex_32(&value, "invalid_recipient_key"))?;
    if decoded.group_id != expected_mailbox {
        return Err(ApiError::bad_request("direct_recipient_mismatch"));
    }
    let scope_id = direct_scope_id(
        &request.event.author_public_key,
        &request.recipient_public_key,
    )
    .map_err(|_| ApiError::bad_request("invalid_direct_participants"))?;
    let decoded_scope_id = decode_hex_32(&scope_id, "invalid_direct_participants")?;
    let signed_wire_record = serde_json::to_vec(&request.event).map_err(ApiError::database)?;

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;

    if let Some(existing) = transaction
        .query_opt(
            "SELECT canonical_cursor, scope_kind, protocol_scope_id, signed_wire_record
             FROM noise.events
             WHERE event_id = $1",
            &[&decoded.event_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        let existing_scope_kind: &str = existing.get(1);
        let existing_scope_id: Vec<u8> = existing.get(2);
        let existing_wire: Vec<u8> = existing.get(3);
        if existing_scope_kind != "direct"
            || existing_scope_id != decoded_scope_id
            || existing_wire != signed_wire_record
        {
            return Err(ApiError::conflict("event_id_conflict"));
        }
        let cursor: i64 = existing.get(0);
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok((
            StatusCode::OK,
            Json(DirectEventAcceptance {
                status: "accepted",
                event_id: request.event.event_id,
                direct_scope_id: scope_id,
                canonical_cursor: cursor,
            }),
        ));
    }

    let recipient = transaction
        .query_opt(
            "SELECT a.account_id
             FROM noise.accounts a
             LEFT JOIN noise.account_restrictions ar
               ON ar.account_id = a.account_id
              AND (ar.expires_at IS NULL OR ar.expires_at > clock_timestamp())
             WHERE a.identity_public_key = $1
               AND a.status = 'active'
               AND ar.account_id IS NULL",
            &[&recipient_public_key.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("direct_recipient_unavailable"))?;
    let recipient_account_id: i64 = recipient.get(0);
    let (account_low_id, account_high_id) = if session.account_id < recipient_account_id {
        (session.account_id, recipient_account_id)
    } else {
        (recipient_account_id, session.account_id)
    };
    transaction
        .execute(
            "INSERT INTO noise.direct_threads (
                protocol_scope_id, account_low_id, account_high_id
             ) VALUES ($1, $2, $3)
             ON CONFLICT (account_low_id, account_high_id) DO NOTHING",
            &[
                &decoded_scope_id.as_slice(),
                &account_low_id,
                &account_high_id,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    let thread = transaction
        .query_one(
            "SELECT direct_thread_pk, protocol_scope_id
             FROM noise.direct_threads
             WHERE account_low_id = $1 AND account_high_id = $2
             FOR UPDATE",
            &[&account_low_id, &account_high_id],
        )
        .await
        .map_err(ApiError::database)?;
    let direct_thread_pk: i64 = thread.get(0);
    let stored_scope_id: Vec<u8> = thread.get(1);
    if stored_scope_id != decoded_scope_id {
        return Err(ApiError::conflict("direct_thread_scope_conflict"));
    }
    let stream_pk = ensure_direct_stream(&transaction, direct_thread_pk, &decoded_scope_id).await?;
    if let Some(existing) = transaction
        .query_opt(
            "SELECT canonical_cursor, scope_kind, protocol_scope_id, signed_wire_record
             FROM noise.events
             WHERE event_id = $1",
            &[&decoded.event_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
    {
        let existing_scope_kind: &str = existing.get(1);
        let existing_scope_id: Vec<u8> = existing.get(2);
        let existing_wire: Vec<u8> = existing.get(3);
        if existing_scope_kind != "direct"
            || existing_scope_id != decoded_scope_id
            || existing_wire != signed_wire_record
        {
            return Err(ApiError::conflict("event_id_conflict"));
        }
        let cursor: i64 = existing.get(0);
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok((
            StatusCode::OK,
            Json(DirectEventAcceptance {
                status: "accepted",
                event_id: request.event.event_id,
                direct_scope_id: scope_id,
                canonical_cursor: cursor,
            }),
        ));
    }
    let cursor_row = transaction
        .query_one(
            "UPDATE noise.cursor_clock
             SET last_cursor = last_cursor + 1
             WHERE singleton
             RETURNING last_cursor",
            &[],
        )
        .await
        .map_err(ApiError::database)?;
    let canonical_cursor: i64 = cursor_row.get(0);
    let author_sequence = request.event.author_sequence.to_string();
    let created_at_millis = request.event.created_at_millis.to_string();
    transaction
        .execute(
            "INSERT INTO noise.events (
                event_id, canonical_cursor, scope_kind, protocol_scope_id,
                direct_thread_pk, stream_pk, author_account_id, author_sequence,
                created_at_millis, encryption_version, nonce, ciphertext,
                signature, signed_wire_record
             ) VALUES (
                $1, $2, 'direct', $3, $4, $5, $6, $7::text::numeric,
                $8::text::numeric, 1, $9, $10, $11, $12
             )",
            &[
                &decoded.event_id.as_slice(),
                &canonical_cursor,
                &decoded_scope_id.as_slice(),
                &direct_thread_pk,
                &stream_pk,
                &session.account_id,
                &author_sequence,
                &created_at_millis,
                &decoded.nonce.as_slice(),
                &decoded.ciphertext,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(event_conflict)?;
    transaction
        .execute(
            "UPDATE noise.streams
             SET latest_cursor = $2
             WHERE stream_pk = $1",
            &[&stream_pk, &canonical_cursor],
        )
        .await
        .map_err(ApiError::database)?;
    let outbox_payload = serde_json::to_vec(&serde_json::json!({
        "canonical_cursor": canonical_cursor,
        "event_id": request.event.event_id,
        "direct_scope_id": scope_id,
    }))
    .map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.outbox_events (
                topic, aggregate_kind, aggregate_id, payload
             ) VALUES ('event.accepted', 'event', $1, $2)",
            &[&decoded.event_id.as_slice(), &outbox_payload],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;

    Ok((
        StatusCode::CREATED,
        Json(DirectEventAcceptance {
            status: "accepted",
            event_id: request.event.event_id,
            direct_scope_id: scope_id,
            canonical_cursor,
        }),
    ))
}

pub async fn direct_events_after(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DirectEventsAfterQuery>,
    headers: HeaderMap,
) -> Result<Json<CanonicalEventPage>, ApiError> {
    if query.after < 0 {
        return Err(ApiError::bad_request("invalid_cursor"));
    }
    let limit = page_limit(query.limit)?;
    let session = authenticate_session(&state, &headers).await?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let Some(stream) = find_direct_stream(&client, &session, &query.peer_public_key).await? else {
        return Ok(Json(CanonicalEventPage {
            events: Vec::new(),
            next_cursor: query.after,
            has_more: false,
        }));
    };
    let mut events = read_events_after(
        &client,
        stream.stream_pk,
        query.after,
        stream.latest_cursor,
        limit + 1,
    )
    .await?;
    let has_more = events.len() > limit as usize;
    if has_more {
        events.pop();
    }
    let next_cursor = if has_more {
        events
            .last()
            .map_or(query.after, |event| event.canonical_cursor)
    } else {
        query.after.max(stream.latest_cursor)
    };
    Ok(Json(CanonicalEventPage {
        events,
        next_cursor,
        has_more,
    }))
}

pub async fn latest_direct_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LatestDirectEventsQuery>,
    headers: HeaderMap,
) -> Result<Json<CanonicalEventPage>, ApiError> {
    let limit = page_limit(query.limit)?;
    let session = authenticate_session(&state, &headers).await?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let Some(stream) = find_direct_stream(&client, &session, &query.peer_public_key).await? else {
        return Ok(Json(CanonicalEventPage {
            events: Vec::new(),
            next_cursor: 0,
            has_more: false,
        }));
    };
    let mut events =
        read_latest_events(&client, stream.stream_pk, stream.latest_cursor, limit + 1).await?;
    let has_more = events.len() > limit as usize;
    if has_more {
        events.remove(0);
    }
    Ok(Json(CanonicalEventPage {
        events,
        next_cursor: stream.latest_cursor,
        has_more,
    }))
}

struct DecodedEvent {
    event_id: [u8; 32],
    group_id: [u8; 32],
    author_public_key: [u8; 32],
    stream_locator: Option<[u8; 32]>,
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
    signature: [u8; 64],
}

impl DecodedEvent {
    fn new(event: &SignedEvent) -> Result<Self, ApiError> {
        if event.author_sequence == 0 {
            return Err(ApiError::bad_request("invalid_event"));
        }
        let event_id = decode_hex_32(&event.event_id, "invalid_event")?;
        let group_id = decode_hex_32(&event.group_id, "invalid_event")?;
        let author_public_key =
            decode_canonical_array::<32>(&event.author_public_key, "invalid_event")?;
        let stream_locator = event
            .stream_locator
            .as_deref()
            .map(|value| decode_hex_32(value, "invalid_event"))
            .transpose()?;
        let nonce = decode_canonical_array::<24>(&event.nonce_base64, "invalid_event")?;
        let ciphertext = STANDARD_NO_PAD
            .decode(&event.ciphertext_base64)
            .map_err(|_| ApiError::bad_request("invalid_event"))?;
        if STANDARD_NO_PAD.encode(&ciphertext) != event.ciphertext_base64
            || ciphertext.len() <= 16
            || ciphertext.len() > MAX_EVENT_CIPHERTEXT_BYTES
        {
            return Err(ApiError::bad_request("invalid_event"));
        }
        let signature = decode_canonical_array::<64>(&event.signature_base64, "invalid_event")?;
        Ok(Self {
            event_id,
            group_id,
            author_public_key,
            stream_locator,
            nonce,
            ciphertext,
            signature,
        })
    }
}

struct AuthorizedGroup {
    group_pk: i64,
}

async fn authorize_group<C: GenericClient + Sync>(
    client: &C,
    session: &AuthenticatedSession,
    group_id: &[u8; 32],
    lock: bool,
) -> Result<AuthorizedGroup, ApiError> {
    let lock_clause = if lock { " FOR UPDATE OF g" } else { "" };
    let statement = format!(
        "SELECT g.group_pk
         FROM noise.groups g
         JOIN noise.group_memberships gm
           ON gm.group_pk = g.group_pk
          AND gm.account_id = $2
          AND gm.active_until_cursor IS NULL
         LEFT JOIN noise.group_restrictions gr
           ON gr.group_pk = g.group_pk
          AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
         WHERE g.protocol_group_id = $1
           AND g.lifecycle_state = 'active'
           AND g.safety_state = 'normal'
           AND gr.group_pk IS NULL{lock_clause}"
    );
    let row = client
        .query_opt(&statement, &[&group_id.as_slice(), &session.account_id])
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("group_unavailable"))?;
    Ok(AuthorizedGroup {
        group_pk: row.get(0),
    })
}

async fn validate_event_epoch(
    transaction: &Transaction<'_>,
    session: &AuthenticatedSession,
    group_pk: i64,
    event: &SignedEvent,
) -> Result<(), ApiError> {
    let genesis = transaction
        .query_opt(
            "SELECT founder_account_id
             FROM noise.mls_geneses
             WHERE group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let Some(genesis) = genesis else {
        if event.encryption_version == 1 && event.epoch.is_none() && event.stream_locator.is_none()
        {
            return Ok(());
        }
        return Err(ApiError::conflict("group_has_not_enabled_mls"));
    };
    if !matches!(event.encryption_version, 2 | 3) {
        return Err(ApiError::conflict("legacy_events_closed"));
    }
    let epoch = event
        .epoch
        .ok_or_else(|| ApiError::bad_request("invalid_event_epoch"))?;
    if epoch == 0 {
        if genesis.get::<_, i64>(0) != session.account_id {
            return Err(ApiError::conflict("event_author_not_in_epoch"));
        }
        return Ok(());
    }
    let epoch_text = epoch.to_string();
    let member = transaction
        .query_opt(
            "SELECT 1
             FROM noise.mls_epochs e
             JOIN noise.mls_epoch_members m ON m.epoch_record_id = e.record_id
             WHERE e.group_pk = $1
               AND e.epoch = $2::text::numeric
               AND m.account_id = $3",
            &[&group_pk, &epoch_text, &session.account_id],
        )
        .await
        .map_err(ApiError::database)?;
    if member.is_none() {
        return Err(ApiError::conflict("event_author_not_in_epoch"));
    }
    Ok(())
}

async fn ensure_stream(
    transaction: &Transaction<'_>,
    group_pk: i64,
    stream_locator: Option<&[u8; 32]>,
) -> Result<i64, ApiError> {
    if let Some(locator) = stream_locator {
        if let Some(row) = transaction
            .query_opt(
                "SELECT stream_pk
                 FROM noise.streams
                 WHERE group_pk = $1
                   AND stream_kind = 'topic'
                   AND protocol_stream_locator = $2",
                &[&group_pk, &locator.as_slice()],
            )
            .await
            .map_err(ApiError::database)?
        {
            return Ok(row.get(0));
        }
        return transaction
            .query_one(
                "INSERT INTO noise.streams (
                    stream_kind, group_pk, protocol_stream_locator
                 ) VALUES ('topic', $1, $2)
                 RETURNING stream_pk",
                &[&group_pk, &locator.as_slice()],
            )
            .await
            .map(|row| row.get(0))
            .map_err(ApiError::database);
    }
    if let Some(row) = transaction
        .query_opt(
            "SELECT stream_pk
             FROM noise.streams
             WHERE group_pk = $1 AND stream_kind = 'group'",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?
    {
        return Ok(row.get(0));
    }
    transaction
        .query_one(
            "INSERT INTO noise.streams (stream_kind, group_pk)
             VALUES ('group', $1)
             RETURNING stream_pk",
            &[&group_pk],
        )
        .await
        .map(|row| row.get(0))
        .map_err(ApiError::database)
}

async fn ensure_direct_stream(
    transaction: &Transaction<'_>,
    direct_thread_pk: i64,
    scope_id: &[u8; 32],
) -> Result<i64, ApiError> {
    if let Some(row) = transaction
        .query_opt(
            "SELECT stream_pk
             FROM noise.streams
             WHERE direct_thread_pk = $1 AND stream_kind = 'direct'",
            &[&direct_thread_pk],
        )
        .await
        .map_err(ApiError::database)?
    {
        return Ok(row.get(0));
    }
    transaction
        .query_one(
            "INSERT INTO noise.streams (
                stream_kind, direct_thread_pk, protocol_stream_locator
             ) VALUES ('direct', $1, $2)
             RETURNING stream_pk",
            &[&direct_thread_pk, &scope_id.as_slice()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(ApiError::database)
}

struct Stream {
    stream_pk: i64,
    latest_cursor: i64,
}

async fn find_direct_stream<C: GenericClient + Sync>(
    client: &C,
    session: &AuthenticatedSession,
    peer_public_key: &str,
) -> Result<Option<Stream>, ApiError> {
    let peer_public_key = decode_canonical_array::<32>(peer_public_key, "invalid_peer_key")?;
    if peer_public_key == session.identity_public_key {
        return Err(ApiError::bad_request("invalid_direct_peer"));
    }
    let peer_public_key_base64 = STANDARD_NO_PAD.encode(peer_public_key);
    let self_public_key_base64 = STANDARD_NO_PAD.encode(session.identity_public_key);
    let scope_id = direct_scope_id(&self_public_key_base64, &peer_public_key_base64)
        .map_err(|_| ApiError::bad_request("invalid_direct_participants"))
        .and_then(|value| decode_hex_32(&value, "invalid_direct_participants"))?;
    let row = client
        .query_opt(
            "SELECT s.stream_pk, s.latest_cursor
             FROM noise.accounts peer
             JOIN noise.direct_threads dt
               ON dt.account_low_id = LEAST($1, peer.account_id)
              AND dt.account_high_id = GREATEST($1, peer.account_id)
              AND dt.protocol_scope_id = $3
             JOIN noise.streams s
               ON s.direct_thread_pk = dt.direct_thread_pk
              AND s.stream_kind = 'direct'
             WHERE peer.identity_public_key = $2",
            &[
                &session.account_id,
                &peer_public_key.as_slice(),
                &scope_id.as_slice(),
            ],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(row.map(|row| Stream {
        stream_pk: row.get(0),
        latest_cursor: row.get(1),
    }))
}

async fn find_stream<C: GenericClient + Sync>(
    client: &C,
    group_pk: i64,
    locator: Option<&[u8; 32]>,
) -> Result<Option<Stream>, ApiError> {
    let row = if let Some(locator) = locator {
        client
            .query_opt(
                "SELECT stream_pk, latest_cursor
                 FROM noise.streams
                 WHERE group_pk = $1
                   AND stream_kind = 'topic'
                   AND protocol_stream_locator = $2",
                &[&group_pk, &locator.as_slice()],
            )
            .await
    } else {
        client
            .query_opt(
                "SELECT stream_pk, latest_cursor
                 FROM noise.streams
                 WHERE group_pk = $1 AND stream_kind = 'group'",
                &[&group_pk],
            )
            .await
    }
    .map_err(ApiError::database)?;
    Ok(row.map(|row| Stream {
        stream_pk: row.get(0),
        latest_cursor: row.get(1),
    }))
}

async fn read_events_after<C: GenericClient + Sync>(
    client: &C,
    stream_pk: i64,
    after: i64,
    high_water: i64,
    limit: u16,
) -> Result<Vec<CanonicalEvent>, ApiError> {
    let rows = client
        .query(
            "SELECT e.canonical_cursor, e.signed_wire_record
             FROM noise.events e
             LEFT JOIN noise.event_restrictions er
               ON er.event_id = e.event_id
              AND (er.expires_at IS NULL OR er.expires_at > clock_timestamp())
             WHERE e.stream_pk = $1
               AND e.canonical_cursor > $2
               AND e.canonical_cursor <= $3
               AND e.hidden_at IS NULL
               AND er.event_id IS NULL
             ORDER BY e.canonical_cursor ASC
             LIMIT $4",
            &[&stream_pk, &after, &high_water, &(limit as i64)],
        )
        .await
        .map_err(ApiError::database)?;
    rows.into_iter().map(canonical_event).collect()
}

async fn read_latest_events<C: GenericClient + Sync>(
    client: &C,
    stream_pk: i64,
    high_water: i64,
    limit: u16,
) -> Result<Vec<CanonicalEvent>, ApiError> {
    let rows = client
        .query(
            "SELECT canonical_cursor, signed_wire_record
             FROM (
                SELECT e.canonical_cursor, e.signed_wire_record
                FROM noise.events e
                LEFT JOIN noise.event_restrictions er
                  ON er.event_id = e.event_id
                 AND (er.expires_at IS NULL OR er.expires_at > clock_timestamp())
                WHERE e.stream_pk = $1
                  AND e.canonical_cursor <= $2
                  AND e.hidden_at IS NULL
                  AND er.event_id IS NULL
                ORDER BY e.canonical_cursor DESC
                LIMIT $3
             ) latest
             ORDER BY canonical_cursor ASC",
            &[&stream_pk, &high_water, &(limit as i64)],
        )
        .await
        .map_err(ApiError::database)?;
    rows.into_iter().map(canonical_event).collect()
}

fn canonical_event(row: tokio_postgres::Row) -> Result<CanonicalEvent, ApiError> {
    let wire: Vec<u8> = row.get(1);
    let event: SignedEvent = serde_json::from_slice(&wire).map_err(ApiError::database)?;
    event.verify().map_err(ApiError::database)?;
    Ok(CanonicalEvent {
        canonical_cursor: row.get(0),
        event,
    })
}

fn page_limit(limit: Option<u16>) -> Result<u16, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(ApiError::bad_request("invalid_page_limit"));
    }
    Ok(limit)
}

fn event_conflict(error: tokio_postgres::Error) -> ApiError {
    if error
        .as_db_error()
        .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION)
    {
        ApiError::conflict("event_sequence_conflict")
    } else {
        ApiError::database(error)
    }
}
