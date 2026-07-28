BEGIN;

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';
SET LOCAL search_path = noise, public;

ALTER TABLE noise.events
    ADD COLUMN expires_after_read_seconds integer
        CHECK (expires_after_read_seconds IS NULL
            OR expires_after_read_seconds BETWEEN 60 AND 2419200);

CREATE TABLE noise.direct_event_receipts (
    event_id bytea PRIMARY KEY
        REFERENCES noise.events(event_id) ON DELETE CASCADE,
    recipient_account_id bigint NOT NULL
        REFERENCES noise.accounts(account_id),
    delivered_at timestamptz,
    read_at timestamptz,
    expires_at timestamptz,
    CHECK (read_at IS NULL OR delivered_at IS NOT NULL),
    CHECK (expires_at IS NULL OR read_at IS NOT NULL),
    CHECK (read_at IS NULL OR read_at >= delivered_at),
    CHECK (expires_at IS NULL OR expires_at > read_at)
);

INSERT INTO noise.direct_event_receipts (event_id, recipient_account_id)
SELECT
    event.event_id,
    CASE
        WHEN event.author_account_id = thread.account_low_id
            THEN thread.account_high_id
        ELSE thread.account_low_id
    END
FROM noise.events event
JOIN noise.direct_threads thread
  ON thread.direct_thread_pk = event.direct_thread_pk
WHERE event.scope_kind = 'direct';

CREATE INDEX direct_event_receipts_recipient_idx
    ON noise.direct_event_receipts (recipient_account_id, event_id);

CREATE INDEX direct_event_receipts_expiry_idx
    ON noise.direct_event_receipts (expires_at)
    WHERE expires_at IS NOT NULL;

INSERT INTO noise.schema_migrations (version, name)
VALUES (5, 'direct_receipts_and_expiry');

COMMIT;
