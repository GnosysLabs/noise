BEGIN;

SET LOCAL search_path = noise, public;

ALTER TABLE noise.push_subscriptions
    DROP CONSTRAINT IF EXISTS push_subscriptions_token_lookup_hash_key;

CREATE INDEX IF NOT EXISTS push_subscriptions_token_lookup_idx
    ON noise.push_subscriptions (token_lookup_hash)
    WHERE revoked_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS push_subscriptions_device_provider_active_idx
    ON noise.push_subscriptions (device_pk, provider)
    WHERE revoked_at IS NULL;

INSERT INTO noise.schema_migrations (version, name)
VALUES (8, 'central_push_notifications');

COMMIT;
