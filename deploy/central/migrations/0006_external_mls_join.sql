BEGIN;

CREATE TABLE noise.mls_external_join_packages (
    package_id bytea PRIMARY KEY CHECK (octet_length(package_id) = 32),
    group_pk bigint NOT NULL UNIQUE REFERENCES noise.groups(group_pk),
    epoch numeric(20, 0) NOT NULL
        CHECK (epoch >= 0 AND epoch <= 18446744073709551615),
    control_record_id bytea NOT NULL CHECK (octet_length(control_record_id) = 32),
    publisher_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

INSERT INTO noise.schema_migrations (version, name)
VALUES (6, 'external_mls_join');

COMMIT;
