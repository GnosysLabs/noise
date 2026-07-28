//! Scoped realtime change tracking.
//!
//! Every write that should wake a `/v1/groups/{scope}/watch/{since}` long-poll
//! records a row in `noise.watch_changes` inside the same transaction, keyed by
//! the watch scope (a group id or a direct mailbox id) and stamped with a
//! change id drawn from the shared cursor clock. Watchers read per-scope
//! deltas, so an event in one group no longer wakes every watcher on the
//! server, and the response can name exactly which streams moved.
//!
//! After the transaction commits, the caller pings [`WatchNotifier`] so
//! in-process long-polls wake immediately instead of waiting for their
//! fallback database poll.

use std::collections::HashMap;
use std::sync::Arc;

use deadpool_postgres::GenericClient;
use tokio::sync::{Notify, RwLock};

use super::error::ApiError;

/// How many change rows are retained per scope. A watcher whose `since` falls
/// behind the retained window is told the hints are incomplete and performs a
/// full reconcile, mirroring the relay implementation's ring history.
const WATCH_CHANGE_HISTORY_LIMIT: i64 = 512;

#[derive(Default)]
pub struct WatchNotifier {
    scopes: RwLock<HashMap<String, Arc<Notify>>>,
}

impl WatchNotifier {
    /// Returns the wake handle for a scope, creating it on first use. Watchers
    /// must obtain the handle and register interest *before* querying the
    /// database so a change committed between the query and the wait still
    /// wakes them through the fallback poll at worst.
    pub async fn handle(&self, scope_id: &str) -> Arc<Notify> {
        if let Some(existing) = self.scopes.read().await.get(scope_id) {
            return existing.clone();
        }
        let mut scopes = self.scopes.write().await;
        scopes
            .entry(scope_id.to_owned())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    /// Wakes every in-process watcher of a scope. Callers invoke this after
    /// the recording transaction commits; a notification with no waiters is a
    /// no-op, and cross-process deployments are covered by the watchers'
    /// fallback poll.
    pub async fn wake(&self, scope_id: &str) {
        if let Some(notify) = self.scopes.read().await.get(scope_id) {
            notify.notify_waiters();
        }
    }
}

/// One recorded change, scoped to a watch id.
pub struct WatchChange<'a> {
    /// The watch scope: a group id or direct mailbox id in canonical hex.
    pub scope_id: &'a str,
    /// The changed stream's locator in canonical hex; `None` for the default
    /// (general) stream and for control-plane changes. Watch responses map
    /// `None` onto `control_changed`, which clients answer with a group-level
    /// activity sync that covers both general messages and control events.
    pub stream_locator: Option<&'a str>,
    /// Whether the change affects control state (membership, MLS, deletion).
    pub control: bool,
}

/// Records a change using an already-allocated cursor value (an event's
/// canonical cursor). The cursor clock is a single ordered domain, so event
/// cursors and standalone change ids interleave correctly.
pub async fn record_watch_change_at<C: GenericClient + Sync>(
    client: &C,
    change_id: i64,
    change: WatchChange<'_>,
) -> Result<(), ApiError> {
    client
        .execute(
            "INSERT INTO noise.watch_changes (
                change_id, scope_id, stream_locator, control
             ) VALUES ($1, $2, $3, $4)",
            &[
                &change_id,
                &change.scope_id,
                &change.stream_locator,
                &change.control,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    prune_watch_changes(client, change.scope_id).await
}

/// Records a change, drawing a fresh id from the shared cursor clock. Used by
/// writes that do not already carry a canonical event cursor (receipts, MLS
/// operations, deletions).
pub async fn record_watch_change<C: GenericClient + Sync>(
    client: &C,
    change: WatchChange<'_>,
) -> Result<(), ApiError> {
    let row = client
        .query_one(
            "UPDATE noise.cursor_clock
             SET last_cursor = last_cursor + 1
             WHERE singleton
             RETURNING last_cursor",
            &[],
        )
        .await
        .map_err(ApiError::database)?;
    let change_id: i64 = row.get(0);
    record_watch_change_at(client, change_id, change).await
}

async fn prune_watch_changes<C: GenericClient + Sync>(
    client: &C,
    scope_id: &str,
) -> Result<(), ApiError> {
    client
        .execute(
            "DELETE FROM noise.watch_changes
             WHERE scope_id = $1
               AND change_id < (
                 SELECT COALESCE(min(change_id), 0) FROM (
                   SELECT change_id FROM noise.watch_changes
                   WHERE scope_id = $1
                   ORDER BY change_id DESC
                   LIMIT $2
                 ) retained)",
            &[&scope_id, &WATCH_CHANGE_HISTORY_LIMIT],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(())
}
