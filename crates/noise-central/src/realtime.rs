use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use noise_core::{GroupPresence, direct_mailbox_id};
use serde::Serialize;

use super::{AppState, AuthenticatedSession, authenticate_session, decode_hex_32, error::ApiError};

const WATCH_TIMEOUT: Duration = Duration::from_secs(20);
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_GROUP_PRESENCE_MILLIS: u64 = 5 * 60_000;
const RECENT_GROUP_PRESENCE_MILLIS: u64 = 15 * 60_000;
const MAX_GROUP_PRESENCES: usize = 10_000;

#[derive(Serialize)]
pub struct GroupWatchResponse {
    revision: u64,
    changed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    presences: Vec<GroupPresence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_stream_locators: Vec<String>,
    control_changed: bool,
    change_hints_complete: bool,
}

pub async fn account_watch(
    State(state): State<Arc<AppState>>,
    Path((locator, since)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GroupWatchResponse>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let locator = decode_hex_32(&locator, "invalid_vault_locator")?;
    let since = parse_revision(&since)?;
    let started = tokio::time::Instant::now();
    loop {
        let client = state
            .database
            .pool
            .get()
            .await
            .map_err(ApiError::database)?;
        let row = client
            .query_opt(
                "SELECT h.revision::text, v.deleted
                 FROM noise.account_vault_locators l
                 JOIN noise.account_vault_heads h ON h.locator = l.locator
                 JOIN noise.account_vault_versions v
                   ON v.locator = h.locator AND v.revision = h.revision
                 WHERE l.locator = $1 AND l.account_id = $2",
                &[&locator.as_slice(), &session.account_id],
            )
            .await
            .map_err(ApiError::database)?;
        if let Some(row) = row {
            let revision = row
                .get::<_, &str>(0)
                .parse::<u64>()
                .map_err(ApiError::database)?;
            if row.get::<_, bool>(1) {
                return Err(ApiError::gone("account_deleted"));
            }
            if revision > since || started.elapsed() >= WATCH_TIMEOUT {
                return Ok(Json(empty_watch(revision, revision > since)));
            }
        } else if started.elapsed() >= WATCH_TIMEOUT {
            return Ok(Json(empty_watch(since, false)));
        }
        tokio::time::sleep(WATCH_POLL_INTERVAL).await;
    }
}

pub async fn group_watch(
    State(state): State<Arc<AppState>>,
    Path((group_id, since)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GroupWatchResponse>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    decode_hex_32(&group_id, "invalid_group_id")?;
    authorize_watch_scope(&state, &session, &group_id).await?;
    let since = parse_revision(&since)?;
    let started = tokio::time::Instant::now();
    loop {
        let revision = global_revision(&state).await?;
        if revision > since || started.elapsed() >= WATCH_TIMEOUT {
            return Ok(Json(GroupWatchResponse {
                revision,
                changed: revision > since,
                presences: active_presences(&state, &group_id).await,
                changed_stream_locators: Vec::new(),
                control_changed: revision > since,
                change_hints_complete: false,
            }));
        }
        tokio::time::sleep(WATCH_POLL_INTERVAL).await;
    }
}

pub async fn publish_presence(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
    Json(presence): Json<GroupPresence>,
) -> Result<StatusCode, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    decode_hex_32(&group_id, "invalid_group_id")?;
    authorize_presence_scope(&state, &session, &group_id).await?;
    if presence.group_id != group_id
        || presence.member_tag_base64.len() != 43
        || presence.member_nonce_base64.len() != 32
        || presence.member_ciphertext_base64.len() > 128
        || presence.signature_base64.len() != 86
    {
        return Err(ApiError::bad_request("invalid_presence"));
    }
    let now = current_millis()?;
    if presence.expires_at_millis <= now
        || presence.expires_at_millis > now.saturating_add(MAX_GROUP_PRESENCE_MILLIS)
    {
        return Err(ApiError::bad_request("invalid_presence_expiry"));
    }
    let mut groups = state.presences.write().await;
    let group = groups.entry(group_id).or_default();
    group.retain(|_, current| {
        current
            .expires_at_millis
            .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
            > now
    });
    if group.len() >= MAX_GROUP_PRESENCES && !group.contains_key(&presence.member_tag_base64) {
        return Err(ApiError::too_many_requests());
    }
    group.insert(presence.member_tag_base64.clone(), presence);
    Ok(StatusCode::NO_CONTENT)
}

async fn authorize_watch_scope(
    state: &AppState,
    session: &AuthenticatedSession,
    group_id: &str,
) -> Result<(), ApiError> {
    let self_public_key = STANDARD_NO_PAD.encode(session.identity_public_key);
    if direct_mailbox_id(&self_public_key).ok().as_deref() == Some(group_id) {
        return Ok(());
    }
    let group_id = decode_hex_32(group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_opt(
            "SELECT g.lifecycle_state
             FROM noise.groups g
             JOIN noise.group_memberships gm
               ON gm.group_pk = g.group_pk
              AND gm.account_id = $2
              AND gm.active_until_cursor IS NULL
             WHERE g.protocol_group_id = $1",
            &[&group_id.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    if row.get::<_, &str>(0) == "deleted" {
        return Err(ApiError::gone("group_deleted"));
    }
    Ok(())
}

async fn authorize_presence_scope(
    state: &AppState,
    session: &AuthenticatedSession,
    group_id: &str,
) -> Result<(), ApiError> {
    let group_bytes = decode_hex_32(group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    if client
        .query_opt(
            "SELECT 1
             FROM noise.groups g
             JOIN noise.group_memberships gm
               ON gm.group_pk = g.group_pk
              AND gm.account_id = $2
              AND gm.active_until_cursor IS NULL
             WHERE g.protocol_group_id = $1
               AND g.lifecycle_state = 'active'",
            &[&group_bytes.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .is_some()
    {
        return Ok(());
    }
    let rows = client
        .query(
            "SELECT identity_public_key
             FROM noise.accounts
             WHERE status = 'active'",
            &[],
        )
        .await
        .map_err(ApiError::database)?;
    if rows.into_iter().any(|row| {
        let public_key = STANDARD_NO_PAD.encode(row.get::<_, Vec<u8>>(0));
        direct_mailbox_id(&public_key).ok().as_deref() == Some(group_id)
    }) {
        return Ok(());
    }
    Err(ApiError::forbidden())
}

async fn global_revision(state: &AppState) -> Result<u64, ApiError> {
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let revision: i64 = client
        .query_one(
            "SELECT COALESCE(max(outbox_id), 0) FROM noise.outbox_events",
            &[],
        )
        .await
        .map_err(ApiError::database)?
        .get(0);
    u64::try_from(revision).map_err(ApiError::database)
}

async fn active_presences(state: &AppState, group_id: &str) -> Vec<GroupPresence> {
    let Ok(now) = current_millis() else {
        return Vec::new();
    };
    let mut groups = state.presences.write().await;
    let Some(group) = groups.get_mut(group_id) else {
        return Vec::new();
    };
    group.retain(|_, presence| {
        presence
            .expires_at_millis
            .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
            > now
    });
    let mut values = group.values().cloned().collect::<Vec<_>>();
    values.sort_by(|left, right| left.member_tag_base64.cmp(&right.member_tag_base64));
    if group.is_empty() {
        groups.remove(group_id);
    }
    values
}

fn parse_revision(value: &str) -> Result<u64, ApiError> {
    if value == "initial" {
        return Ok(0);
    }
    value
        .parse::<u64>()
        .map_err(|_| ApiError::bad_request("invalid_watch_revision"))
}

fn empty_watch(revision: u64, changed: bool) -> GroupWatchResponse {
    GroupWatchResponse {
        revision,
        changed,
        presences: Vec::new(),
        changed_stream_locators: Vec::new(),
        control_changed: false,
        change_hints_complete: true,
    }
}

fn current_millis() -> Result<u64, ApiError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(ApiError::database)?
        .as_millis();
    u64::try_from(millis).map_err(ApiError::database)
}
