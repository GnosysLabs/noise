BEGIN;

ALTER TABLE noise.account_vault_locators
    DROP CONSTRAINT IF EXISTS account_vault_locators_account_id_key;

CREATE INDEX IF NOT EXISTS account_vault_locators_account_idx
    ON noise.account_vault_locators (account_id);

ALTER TABLE noise.push_subscriptions
    DROP CONSTRAINT IF EXISTS push_subscriptions_environment_check;

ALTER TABLE noise.push_subscriptions
    ADD CONSTRAINT push_subscriptions_environment_check
    CHECK (environment IN ('production', 'development', 'sandbox'));

INSERT INTO noise.schema_migrations (version, name)
VALUES (2, 'recovery_aliases');

COMMIT;
