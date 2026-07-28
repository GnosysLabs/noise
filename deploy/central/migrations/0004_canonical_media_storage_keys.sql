BEGIN;

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';
SET LOCAL search_path = noise, public;

ALTER TABLE noise.media_blocks
    DROP CONSTRAINT media_blocks_storage_key_check,
    ADD CONSTRAINT media_blocks_storage_key_check
        CHECK (storage_key ~ '^temporary/[A-Za-z0-9/_-]+$'
            OR storage_key ~ '^objects/[A-Za-z0-9/_-]+[.]nsb2$');

INSERT INTO noise.schema_migrations (version, name)
VALUES (4, 'canonical_media_storage_keys');

COMMIT;
