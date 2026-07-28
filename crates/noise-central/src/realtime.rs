use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use noise_core::{GroupPresence, direct_mailbox_id};
use serde::Serialize;

use super::{
    AppState, AuthenticatedSession, authenticate_session, decode_hex_32, encode_hex,
    error::ApiError,
};

const WATCH_TIMEOUT: Duration = Duration::from_secs(20);
// In-process writers wake watchers instantly through WatchNotifier; this poll
// only covers writes from other processes and missed wakeups.
const WATCH_FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(2);
// One more row than a full retained history window, so a response that fills
// the window is recognizably incomplete.
const WATCH_CHANGE_PAGE: i64 = 513;
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
    let notify = state.watch.handle(&vault_watch_scope(&locator)).await;
    let started = tokio::time::Instant::now();
    loop {
        let mut notified = Box::pin(notify.notified());
        notified.as_mut().enable();
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
        drop(client);
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
        wait_for_change(notified, started.elapsed()).await;
    }
}

pub async fn group_watch(
    State(state): State<Arc<AppState>>,
    Path((group_id, since)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GroupWatchResponse>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let scope_id = encode_hex(&decode_hex_32(&group_id, "invalid_group_id")?);
    authorize_watch_scope(&state, &session, &group_id).await?;
    let initial = since == "initial";
    let since = parse_revision(&since)?;
    let since_id = i64::try_from(since).unwrap_or(i64::MAX);
    let notify = state.watch.handle(&scope_id).await;
    let started = tokio::time::Instant::now();
    loop {
        let mut notified = Box::pin(notify.notified());
        notified.as_mut().enable();
        let changes = scope_changes(&state, &scope_id, since_id).await?;
        if !changes.is_empty() {
            return Ok(Json(
                changed_watch(&state, &scope_id, since_id, changes).await?,
            ));
        }
        // "initial" is a catch-up barrier, not a long-poll. Return
        // immediately even when this scope has not changed since the scoped
        // watch table was introduced, so the client can reconcile its local
        // streams before beginning the steady-state watch.
        if initial {
            return Ok(Json(GroupWatchResponse {
                revision: since,
                changed: false,
                presences: active_presences(&state, &scope_id).await,
                changed_stream_locators: Vec::new(),
                control_changed: false,
                change_hints_complete: false,
            }));
        }
        if started.elapsed() >= WATCH_TIMEOUT {
            return Ok(Json(GroupWatchResponse {
                revision: since,
                changed: false,
                presences: active_presences(&state, &scope_id).await,
                changed_stream_locators: Vec::new(),
                control_changed: false,
                change_hints_complete: true,
            }));
        }
        wait_for_change(notified, started.elapsed()).await;
    }
}

struct ScopeChange {
    change_id: i64,
    stream_locator: Option<String>,
    control: bool,
}

async fn scope_changes(
    state: &AppState,
    scope_id: &str,
    since_id: i64,
) -> Result<Vec<ScopeChange>, ApiError> {
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let rows = client
        .query(
            "SELECT change_id, stream_locator, control
             FROM noise.watch_changes
             WHERE scope_id = $1 AND change_id > $2
             ORDER BY change_id
             LIMIT $3",
            &[&scope_id, &since_id, &WATCH_CHANGE_PAGE],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(rows
        .into_iter()
        .map(|row| ScopeChange {
            change_id: row.get(0),
            stream_locator: row.get(1),
            control: row.get(2),
        })
        .collect())
}

async fn changed_watch(
    state: &AppState,
    scope_id: &str,
    since_id: i64,
    changes: Vec<ScopeChange>,
) -> Result<GroupWatchResponse, ApiError> {
    let revision = changes
        .last()
        .map(|change| change.change_id)
        .unwrap_or(since_id);
    // Hints are complete only when the watcher's last-seen change is still
    // inside the retained history window, so nothing can have been pruned out
    // of the gap, and the page itself did not overflow.
    let overflowed = changes.len() >= WATCH_CHANGE_PAGE as usize;
    let covered = if since_id > 0 && !overflowed {
        let client = state
            .database
            .pool
            .get()
            .await
            .map_err(ApiError::database)?;
        let oldest: Option<i64> = client
            .query_one(
                "SELECT min(change_id) FROM noise.watch_changes WHERE scope_id = $1",
                &[&scope_id],
            )
            .await
            .map_err(ApiError::database)?
            .get(0);
        oldest.is_some_and(|oldest| since_id >= oldest)
    } else {
        false
    };
    let control_changed = changes
        .iter()
        .any(|change| change.control || change.stream_locator.is_none());
    let mut changed_stream_locators: Vec<String> = changes
        .iter()
        .filter_map(|change| change.stream_locator.clone())
        .collect();
    changed_stream_locators.sort();
    changed_stream_locators.dedup();
    Ok(GroupWatchResponse {
        revision: u64::try_from(revision).map_err(ApiError::database)?,
        changed: true,
        presences: active_presences(state, scope_id).await,
        changed_stream_locators,
        control_changed,
        change_hints_complete: covered,
    })
}

pub fn vault_watch_scope(locator: &[u8; 32]) -> String {
    // Vault locators and group ids share the 32-byte hex namespace; the prefix
    // keeps an account watch from ever aliasing a group watch scope.
    format!("vault:{}", encode_hex(locator))
}

async fn wait_for_change(
    notified: std::pin::Pin<Box<tokio::sync::futures::Notified<'_>>>,
    elapsed: Duration,
) {
    let remaining = WATCH_TIMEOUT.saturating_sub(elapsed);
    let poll = WATCH_FALLBACK_POLL_INTERVAL
        .min(remaining)
        .max(Duration::from_millis(50));
    tokio::select! {
        _ = notified => {}
        _ = tokio::time::sleep(poll) => {}
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
