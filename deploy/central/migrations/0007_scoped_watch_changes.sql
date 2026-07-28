-- Scoped watch changes: per-group / per-direct-mailbox realtime revisions.
--
-- The previous watch implementation compared clients against the server-wide
-- max(outbox_id), so every watcher woke on every event anywhere. This table
-- records one row per change, scoped to the watch id clients poll
-- (`/v1/groups/{scope}/watch/{since}`), carrying the changed stream locator
-- so clients can resync exactly the streams that moved.

BEGIN;

CREATE TABLE noise.watch_changes (
    change_id bigint PRIMARY KEY,
    scope_id text NOT NULL CHECK (scope_id ~ '^[0-9a-f]{64}$'),
    stream_locator text CHECK (stream_locator ~ '^[0-9a-f]{64}$'),
    control boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX watch_changes_scope_idx
    ON noise.watch_changes (scope_id, change_id);

-- Watch revisions now come from the shared cursor clock. Jump it past the
-- legacy global watch revision domain (max outbox_id) so already-connected
-- clients holding legacy revisions keep observing strictly increasing values
-- and reconcile once instead of stalling on revision <= since.
UPDATE noise.cursor_clock
SET last_cursor = GREATEST(
    last_cursor,
    (SELECT COALESCE(max(outbox_id), 0) + 1 FROM noise.outbox_events)
)
WHERE singleton;

INSERT INTO noise.schema_migrations (version, name)
VALUES (7, 'scoped_watch_changes');

COMMIT;
