BEGIN;

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';
SET LOCAL search_path = noise, public;

CREATE TABLE noise.group_deletions (
    group_pk bigint PRIMARY KEY REFERENCES noise.groups(group_pk),
    owner_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    deleted_at_millis numeric(20, 0) NOT NULL
        CHECK (deleted_at_millis >= 0
            AND deleted_at_millis <= 18446744073709551615),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE noise.contact_signals (
    signal_group_id bytea NOT NULL CHECK (octet_length(signal_group_id) = 32),
    author_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    author_sequence numeric(20, 0) NOT NULL
        CHECK (author_sequence >= 0
            AND author_sequence <= 18446744073709551615),
    event_id bytea NOT NULL UNIQUE CHECK (octet_length(event_id) = 32),
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (signal_group_id, author_account_id)
);

ALTER TABLE noise.group_deletions OWNER TO noise_app;
ALTER TABLE noise.contact_signals OWNER TO noise_app;

ALTER TABLE noise.events
    DROP CONSTRAINT events_author_sequence_check,
    ADD CONSTRAINT events_author_sequence_check
        CHECK (author_sequence >= 0
            AND author_sequence <= 18446744073709551615);

INSERT INTO noise.schema_migrations (version, name)
VALUES (3, 'live_social_operations');

COMMIT;
