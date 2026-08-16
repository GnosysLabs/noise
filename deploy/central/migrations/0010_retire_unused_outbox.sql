BEGIN;

SET LOCAL search_path = noise, public;

-- outbox_events was the pre-watch_changes wakeup log. Every accepted event,
-- receipt, MLS control write, and group deletion inserted a row, but nothing
-- ever set published_at. Watch fan-out now lives in noise.watch_changes
-- (migration 0007). Delete the leftover bookkeeping rows so they cannot be
-- mistaken for a send backlog.
DELETE FROM noise.outbox_events;

INSERT INTO noise.schema_migrations (version, name)
VALUES (10, 'retire_unused_outbox');

COMMIT;
