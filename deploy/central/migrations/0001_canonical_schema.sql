BEGIN;

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

CREATE SCHEMA IF NOT EXISTS noise AUTHORIZATION noise_app;

SET LOCAL search_path = noise, public;

CREATE TABLE noise.schema_migrations (
    version integer PRIMARY KEY CHECK (version > 0),
    name text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9_]+$'),
    applied_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE noise.cursor_clock (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    last_cursor bigint NOT NULL DEFAULT 0 CHECK (last_cursor >= 0)
);

INSERT INTO noise.cursor_clock (singleton, last_cursor) VALUES (true, 0);

CREATE TABLE noise.accounts (
    account_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    identity_public_key bytea NOT NULL UNIQUE
        CHECK (octet_length(identity_public_key) = 32),
    status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'blocked', 'deleted')),
    status_sequence numeric(20, 0) NOT NULL DEFAULT 0
        CHECK (status_sequence >= 0 AND status_sequence <= 18446744073709551615),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    blocked_at timestamptz,
    deleted_at timestamptz,
    CHECK ((status = 'active' AND blocked_at IS NULL AND deleted_at IS NULL)
        OR (status = 'blocked' AND blocked_at IS NOT NULL AND deleted_at IS NULL)
        OR (status = 'deleted' AND deleted_at IS NOT NULL))
);

CREATE TABLE noise.devices (
    device_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    installation_id bytea NOT NULL CHECK (octet_length(installation_id) = 32),
    auth_public_key bytea NOT NULL UNIQUE
        CHECK (octet_length(auth_public_key) = 32),
    registration_version numeric(20, 0) NOT NULL
        CHECK (registration_version > 0
            AND registration_version <= 18446744073709551615),
    registration_challenge_id bytea NOT NULL
        CHECK (octet_length(registration_challenge_id) = 32),
    registration_issued_at_millis numeric(20, 0) NOT NULL
        CHECK (registration_issued_at_millis >= 0
            AND registration_issued_at_millis <= 18446744073709551615),
    registration_signature bytea NOT NULL
        CHECK (octet_length(registration_signature) = 64),
    registered_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_seen_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    revocation_sequence numeric(20, 0) NOT NULL DEFAULT 0
        CHECK (revocation_sequence >= 0
            AND revocation_sequence <= 18446744073709551615),
    revocation_issued_at_millis numeric(20, 0)
        CHECK (revocation_issued_at_millis IS NULL
            OR (revocation_issued_at_millis >= 0
                AND revocation_issued_at_millis <= 18446744073709551615)),
    revocation_signature bytea
        CHECK (revocation_signature IS NULL
            OR octet_length(revocation_signature) = 64),
    UNIQUE (device_pk, account_id),
    UNIQUE (account_id, installation_id),
    CHECK (last_seen_at >= registered_at),
    CHECK ((revoked_at IS NULL
            AND revocation_issued_at_millis IS NULL
            AND revocation_signature IS NULL)
        OR (revoked_at IS NOT NULL
            AND revoked_at >= registered_at
            AND revocation_issued_at_millis IS NOT NULL
            AND revocation_signature IS NOT NULL))
);

CREATE INDEX devices_account_active_idx
    ON noise.devices (account_id, registered_at)
    WHERE revoked_at IS NULL;

CREATE TABLE noise.auth_challenges (
    challenge_id bytea PRIMARY KEY CHECK (octet_length(challenge_id) = 32),
    purpose text NOT NULL
        CHECK (purpose IN ('register_device', 'open_session')),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    device_pk bigint,
    nonce_hash bytea NOT NULL CHECK (octet_length(nonce_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    CHECK (expires_at > created_at),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at),
    CHECK ((purpose = 'register_device' AND device_pk IS NULL)
        OR (purpose = 'open_session' AND device_pk IS NOT NULL)),
    UNIQUE (challenge_id, account_id),
    UNIQUE (challenge_id, account_id, device_pk),
    FOREIGN KEY (device_pk, account_id)
        REFERENCES noise.devices(device_pk, account_id)
);

CREATE INDEX auth_challenges_expiry_idx
    ON noise.auth_challenges (expires_at)
    WHERE consumed_at IS NULL;

ALTER TABLE noise.devices
    ADD CONSTRAINT devices_registration_challenge_fk
    FOREIGN KEY (registration_challenge_id, account_id)
    REFERENCES noise.auth_challenges(challenge_id, account_id);

CREATE UNIQUE INDEX devices_registration_challenge_idx
    ON noise.devices (registration_challenge_id);

CREATE TABLE noise.sessions (
    session_id bytea PRIMARY KEY CHECK (octet_length(session_id) = 32),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    device_pk bigint NOT NULL,
    issued_from_challenge_id bytea NOT NULL UNIQUE,
    issued_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    CHECK (expires_at > issued_at),
    CHECK (revoked_at IS NULL OR revoked_at >= issued_at),
    FOREIGN KEY (device_pk, account_id)
        REFERENCES noise.devices(device_pk, account_id),
    FOREIGN KEY (issued_from_challenge_id, account_id, device_pk)
        REFERENCES noise.auth_challenges(challenge_id, account_id, device_pk)
);

CREATE INDEX sessions_device_active_idx
    ON noise.sessions (device_pk, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE noise.account_vault_locators (
    locator bytea PRIMARY KEY CHECK (octet_length(locator) = 32),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX account_vault_locators_account_idx
    ON noise.account_vault_locators (account_id);

CREATE TABLE noise.account_vault_versions (
    locator bytea NOT NULL CHECK (octet_length(locator) = 32),
    revision numeric(20, 0) NOT NULL
        CHECK (revision > 0 AND revision <= 18446744073709551615),
    nonce bytea,
    ciphertext bytea,
    deleted boolean NOT NULL DEFAULT false,
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (locator, revision),
    FOREIGN KEY (locator) REFERENCES noise.account_vault_locators(locator),
    CHECK ((deleted AND nonce IS NULL AND ciphertext IS NULL)
        OR (NOT deleted
            AND nonce IS NOT NULL
            AND octet_length(nonce) = 24
            AND ciphertext IS NOT NULL
            AND octet_length(ciphertext) > 16))
);

CREATE TABLE noise.account_vault_heads (
    locator bytea PRIMARY KEY CHECK (octet_length(locator) = 32),
    revision numeric(20, 0) NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (locator, revision)
        REFERENCES noise.account_vault_versions(locator, revision)
);

CREATE TABLE noise.groups (
    group_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    protocol_group_id bytea NOT NULL UNIQUE
        CHECK (octet_length(protocol_group_id) = 32),
    founder_account_id bigint REFERENCES noise.accounts(account_id),
    lifecycle_state text NOT NULL DEFAULT 'active'
        CHECK (lifecycle_state IN ('active', 'deleted')),
    safety_state text NOT NULL DEFAULT 'normal'
        CHECK (safety_state IN ('normal', 'paused', 'blocked')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    CHECK ((lifecycle_state = 'active' AND deleted_at IS NULL)
        OR (lifecycle_state = 'deleted' AND deleted_at IS NOT NULL))
);

CREATE TABLE noise.direct_threads (
    direct_thread_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    protocol_scope_id bytea NOT NULL UNIQUE
        CHECK (octet_length(protocol_scope_id) = 32),
    account_low_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    account_high_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (account_low_id < account_high_id),
    UNIQUE (account_low_id, account_high_id)
);

CREATE TABLE noise.streams (
    stream_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    stream_kind text NOT NULL CHECK (stream_kind IN ('group', 'topic', 'direct')),
    group_pk bigint REFERENCES noise.groups(group_pk),
    direct_thread_pk bigint REFERENCES noise.direct_threads(direct_thread_pk),
    protocol_stream_locator bytea
        CHECK (protocol_stream_locator IS NULL
            OR octet_length(protocol_stream_locator) = 32),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    latest_cursor bigint NOT NULL DEFAULT 0 CHECK (latest_cursor >= 0),
    CHECK ((stream_kind IN ('group', 'topic')
            AND group_pk IS NOT NULL
            AND direct_thread_pk IS NULL)
        OR (stream_kind = 'direct'
            AND group_pk IS NULL
            AND direct_thread_pk IS NOT NULL)),
    CHECK ((stream_kind = 'group' AND protocol_stream_locator IS NULL)
        OR (stream_kind IN ('topic', 'direct')
            AND protocol_stream_locator IS NOT NULL))
);

CREATE UNIQUE INDEX streams_group_root_idx
    ON noise.streams (group_pk)
    WHERE stream_kind = 'group';

CREATE UNIQUE INDEX streams_group_locator_idx
    ON noise.streams (group_pk, protocol_stream_locator)
    WHERE stream_kind = 'topic';

CREATE UNIQUE INDEX streams_direct_idx
    ON noise.streams (direct_thread_pk)
    WHERE stream_kind = 'direct';

CREATE TABLE noise.mls_geneses (
    record_id bytea PRIMARY KEY CHECK (octet_length(record_id) = 32),
    group_pk bigint NOT NULL UNIQUE REFERENCES noise.groups(group_pk),
    founder_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    authority_nonce bytea NOT NULL CHECK (octet_length(authority_nonce) = 32),
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE noise.mls_epochs (
    record_id bytea PRIMARY KEY CHECK (octet_length(record_id) = 32),
    group_pk bigint NOT NULL REFERENCES noise.groups(group_pk),
    previous_record_id bytea NOT NULL CHECK (octet_length(previous_record_id) = 32),
    parent_epoch numeric(20, 0) NOT NULL
        CHECK (parent_epoch >= 0 AND parent_epoch <= 18446744073709551615),
    epoch numeric(20, 0) NOT NULL
        CHECK (epoch > 0 AND epoch <= 18446744073709551615),
    author_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (group_pk, epoch),
    UNIQUE (group_pk, previous_record_id),
    CHECK (epoch = parent_epoch + 1)
);

CREATE TABLE noise.mls_epoch_members (
    epoch_record_id bytea NOT NULL REFERENCES noise.mls_epochs(record_id),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    PRIMARY KEY (epoch_record_id, account_id)
);

CREATE TABLE noise.mls_join_requests (
    request_id bytea PRIMARY KEY CHECK (octet_length(request_id) = 32),
    group_pk bigint NOT NULL REFERENCES noise.groups(group_pk),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE noise.mls_removal_requests (
    request_id bytea PRIMARY KEY CHECK (octet_length(request_id) = 32),
    group_pk bigint NOT NULL REFERENCES noise.groups(group_pk),
    requester_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    target_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    reason text NOT NULL CHECK (reason IN ('self_left', 'banned')),
    delete_messages boolean NOT NULL DEFAULT false,
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((reason = 'self_left' AND requester_account_id = target_account_id)
        OR reason = 'banned')
);

CREATE TABLE noise.group_memberships (
    membership_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    group_pk bigint NOT NULL REFERENCES noise.groups(group_pk),
    account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    role text NOT NULL DEFAULT 'member'
        CHECK (role IN ('founder', 'moderator', 'member')),
    source_kind text NOT NULL
        CHECK (source_kind IN ('mls_genesis', 'mls_epoch', 'signed_role', 'legacy_import')),
    source_record_id bytea NOT NULL CHECK (octet_length(source_record_id) = 32),
    active_from_cursor bigint NOT NULL CHECK (active_from_cursor >= 0),
    active_until_cursor bigint CHECK (active_until_cursor IS NULL
        OR active_until_cursor >= active_from_cursor),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE UNIQUE INDEX group_memberships_active_idx
    ON noise.group_memberships (group_pk, account_id)
    WHERE active_until_cursor IS NULL;

CREATE INDEX group_memberships_account_active_idx
    ON noise.group_memberships (account_id, group_pk)
    WHERE active_until_cursor IS NULL;

CREATE TABLE noise.events (
    event_id bytea PRIMARY KEY CHECK (octet_length(event_id) = 32),
    canonical_cursor bigint NOT NULL UNIQUE CHECK (canonical_cursor > 0),
    scope_kind text NOT NULL CHECK (scope_kind IN ('group', 'direct')),
    protocol_scope_id bytea NOT NULL CHECK (octet_length(protocol_scope_id) = 32),
    group_pk bigint REFERENCES noise.groups(group_pk),
    direct_thread_pk bigint REFERENCES noise.direct_threads(direct_thread_pk),
    stream_pk bigint NOT NULL REFERENCES noise.streams(stream_pk),
    author_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    author_sequence numeric(20, 0) NOT NULL
        CHECK (author_sequence >= 0
            AND author_sequence <= 18446744073709551615),
    created_at_millis numeric(20, 0) NOT NULL
        CHECK (created_at_millis >= 0
            AND created_at_millis <= 18446744073709551615),
    encryption_version integer NOT NULL
        CHECK (encryption_version IN (1, 2, 3)),
    epoch numeric(20, 0)
        CHECK (epoch IS NULL
            OR (epoch >= 0 AND epoch <= 18446744073709551615)),
    protocol_stream_locator bytea
        CHECK (protocol_stream_locator IS NULL
            OR octet_length(protocol_stream_locator) = 32),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 24),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) > 16),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    hidden_at timestamptz,
    UNIQUE (protocol_scope_id, author_account_id, author_sequence),
    CHECK ((scope_kind = 'group'
            AND group_pk IS NOT NULL
            AND direct_thread_pk IS NULL)
        OR (scope_kind = 'direct'
            AND group_pk IS NULL
            AND direct_thread_pk IS NOT NULL)),
    CHECK ((encryption_version = 1
            AND epoch IS NULL
            AND protocol_stream_locator IS NULL)
        OR (encryption_version = 2
            AND epoch IS NOT NULL
            AND protocol_stream_locator IS NULL)
        OR (encryption_version = 3
            AND epoch IS NOT NULL
            AND protocol_stream_locator IS NOT NULL))
);

CREATE INDEX events_group_cursor_idx
    ON noise.events (group_pk, canonical_cursor)
    WHERE group_pk IS NOT NULL;

CREATE INDEX events_direct_cursor_idx
    ON noise.events (direct_thread_pk, canonical_cursor)
    WHERE direct_thread_pk IS NOT NULL;

CREATE INDEX events_stream_cursor_idx
    ON noise.events (stream_pk, canonical_cursor);

CREATE TABLE noise.legacy_invitations (
    invitation_id bytea PRIMARY KEY CHECK (octet_length(invitation_id) = 32),
    lookup_hash bytea NOT NULL UNIQUE CHECK (octet_length(lookup_hash) = 32),
    group_pk bigint NOT NULL REFERENCES noise.groups(group_pk),
    generation numeric(20, 0) NOT NULL
        CHECK (generation >= 0 AND generation <= 18446744073709551615),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    expires_at timestamptz,
    consumed_at timestamptz,
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE noise.legacy_invite_rotations (
    rotation_id bytea PRIMARY KEY CHECK (octet_length(rotation_id) = 32),
    invitation_id bytea NOT NULL REFERENCES noise.legacy_invitations(invitation_id),
    generation numeric(20, 0) NOT NULL
        CHECK (generation > 0 AND generation <= 18446744073709551615),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (invitation_id, generation)
);

CREATE TABLE noise.media_objects (
    media_object_id bytea PRIMARY KEY CHECK (octet_length(media_object_id) = 32),
    owner_account_id bigint NOT NULL REFERENCES noise.accounts(account_id),
    state text NOT NULL
        CHECK (state IN ('pending', 'available', 'deleting', 'deleted', 'failed')),
    ciphertext_length bigint NOT NULL CHECK (ciphertext_length >= 0),
    ciphertext_hash bytea NOT NULL CHECK (octet_length(ciphertext_hash) = 32),
    deletion_capability_hash bytea NOT NULL UNIQUE
        CHECK (octet_length(deletion_capability_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finalized_at timestamptz,
    deleted_at timestamptz,
    CHECK ((state = 'pending' AND finalized_at IS NULL AND deleted_at IS NULL)
        OR (state IN ('available', 'deleting') AND finalized_at IS NOT NULL
            AND deleted_at IS NULL)
        OR (state = 'deleted' AND finalized_at IS NOT NULL
            AND deleted_at IS NOT NULL)
        OR (state = 'failed' AND deleted_at IS NULL))
);

CREATE TABLE noise.media_blocks (
    media_object_id bytea NOT NULL REFERENCES noise.media_objects(media_object_id),
    block_index integer NOT NULL CHECK (block_index >= 0),
    storage_key text NOT NULL UNIQUE
        CHECK (storage_key ~ '^temporary/[A-Za-z0-9/_-]+$'
            OR storage_key ~ '^objects/[A-Za-z0-9/_-]+[.]nsb2$'),
    ciphertext_length bigint NOT NULL CHECK (ciphertext_length > 0),
    ciphertext_hash bytea NOT NULL CHECK (octet_length(ciphertext_hash) = 32),
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'available', 'deleted', 'failed')),
    completed_at timestamptz,
    PRIMARY KEY (media_object_id, block_index),
    CHECK ((state = 'pending' AND completed_at IS NULL)
        OR (state IN ('available', 'deleted') AND completed_at IS NOT NULL)
        OR state = 'failed')
);

CREATE TABLE noise.legacy_media_providers (
    provider_id smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider_name text NOT NULL UNIQUE CHECK (provider_name ~ '^[a-z0-9_-]+$'),
    compatibility_origin text NOT NULL UNIQUE
        CHECK (compatibility_origin ~ '^https://[A-Za-z0-9.-]+$')
);

CREATE TABLE noise.legacy_media_objects (
    media_object_id bytea PRIMARY KEY CHECK (octet_length(media_object_id) = 32),
    storage_key text NOT NULL UNIQUE
        CHECK (storage_key ~ '^legacy/objects/[0-9a-f]{2}/[0-9a-f]{64}[.]nsb2$'),
    storage_byte_length bigint NOT NULL CHECK (storage_byte_length > 0),
    storage_payload_hash bytea NOT NULL
        CHECK (octet_length(storage_payload_hash) = 32),
    state text NOT NULL DEFAULT 'available'
        CHECK (state IN ('available', 'deleting', 'deleted', 'failed')),
    imported_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    CHECK ((state IN ('available', 'deleting', 'failed') AND deleted_at IS NULL)
        OR (state = 'deleted' AND deleted_at IS NOT NULL))
);

CREATE TABLE noise.legacy_media_shards (
    provider_id smallint NOT NULL
        REFERENCES noise.legacy_media_providers(provider_id),
    shard_id bytea NOT NULL CHECK (octet_length(shard_id) = 32),
    state text NOT NULL CHECK (state IN ('live', 'deleted')),
    payload_hash bytea
        CHECK (payload_hash IS NULL OR octet_length(payload_hash) = 32),
    ciphertext_length bigint
        CHECK (ciphertext_length IS NULL OR ciphertext_length > 0),
    canonical_object_id bytea
        REFERENCES noise.legacy_media_objects(media_object_id),
    payload_encoding text
        CHECK (payload_encoding IS NULL OR payload_encoding IN ('nsb2', 'legacy_json')),
    deletion_capability_hash bytea
        CHECK (deletion_capability_hash IS NULL
            OR octet_length(deletion_capability_hash) = 32),
    tombstoned_at timestamptz,
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (provider_id, shard_id),
    CHECK ((state = 'live'
            AND payload_hash IS NOT NULL
            AND ciphertext_length IS NOT NULL
            AND canonical_object_id IS NOT NULL
            AND payload_encoding IS NOT NULL
            AND deletion_capability_hash IS NOT NULL
            AND tombstoned_at IS NULL)
        OR (state = 'deleted' AND canonical_object_id IS NULL))
);

CREATE INDEX legacy_media_payload_idx
    ON noise.legacy_media_shards (payload_hash, ciphertext_length)
    WHERE state = 'live';

CREATE INDEX legacy_media_object_alias_idx
    ON noise.legacy_media_shards (canonical_object_id)
    WHERE state = 'live';

CREATE TABLE noise.push_subscriptions (
    push_subscription_pk bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    device_pk bigint NOT NULL REFERENCES noise.devices(device_pk),
    provider text NOT NULL CHECK (provider IN ('apns', 'fcm', 'webpush')),
    environment text NOT NULL
        CHECK (environment IN ('production', 'development', 'sandbox')),
    token_lookup_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_lookup_hash) = 32),
    routing_token_ciphertext bytea NOT NULL
        CHECK (octet_length(routing_token_ciphertext) > 16),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_used_at timestamptz,
    revoked_at timestamptz
);

CREATE INDEX push_subscriptions_device_active_idx
    ON noise.push_subscriptions (device_pk, provider)
    WHERE revoked_at IS NULL;

CREATE TABLE noise.safety_directives (
    directive_id bytea PRIMARY KEY CHECK (octet_length(directive_id) = 32),
    action_set_id bytea NOT NULL CHECK (octet_length(action_set_id) = 32),
    action text NOT NULL CHECK (action IN (
        'hide_event',
        'restore_event',
        'pause_group',
        'block_group',
        'restore_group',
        'block_account',
        'restore_account',
        'delete_media'
    )),
    target_kind text NOT NULL CHECK (target_kind IN ('event', 'group', 'account', 'media')),
    target_id bytea NOT NULL CHECK (octet_length(target_id) = 32),
    reason_code text NOT NULL CHECK (reason_code ~ '^[a-z0-9_]+$'),
    issued_at timestamptz NOT NULL,
    expires_at timestamptz,
    signer_public_key bytea NOT NULL CHECK (octet_length(signer_public_key) = 32),
    signature bytea NOT NULL CHECK (octet_length(signature) = 64),
    signed_wire_record bytea NOT NULL CHECK (octet_length(signed_wire_record) > 0),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (expires_at IS NULL OR expires_at > issued_at),
    CHECK ((action IN ('hide_event', 'restore_event') AND target_kind = 'event')
        OR (action IN ('pause_group', 'block_group', 'restore_group')
            AND target_kind = 'group')
        OR (action IN ('block_account', 'restore_account')
            AND target_kind = 'account')
        OR (action = 'delete_media' AND target_kind = 'media'))
);

CREATE INDEX safety_directives_action_set_idx
    ON noise.safety_directives (action_set_id, issued_at);

CREATE TABLE noise.event_restrictions (
    event_id bytea PRIMARY KEY REFERENCES noise.events(event_id),
    directive_id bytea NOT NULL UNIQUE REFERENCES noise.safety_directives(directive_id),
    active_from timestamptz NOT NULL,
    expires_at timestamptz
);

CREATE TABLE noise.group_restrictions (
    group_pk bigint PRIMARY KEY REFERENCES noise.groups(group_pk),
    directive_id bytea NOT NULL UNIQUE REFERENCES noise.safety_directives(directive_id),
    restriction text NOT NULL CHECK (restriction IN ('paused', 'blocked')),
    active_from timestamptz NOT NULL,
    expires_at timestamptz
);

CREATE TABLE noise.account_restrictions (
    account_id bigint PRIMARY KEY REFERENCES noise.accounts(account_id),
    directive_id bytea NOT NULL UNIQUE REFERENCES noise.safety_directives(directive_id),
    active_from timestamptz NOT NULL,
    expires_at timestamptz
);

CREATE TABLE noise.media_restrictions (
    media_object_id bytea PRIMARY KEY
        REFERENCES noise.media_objects(media_object_id),
    directive_id bytea NOT NULL UNIQUE REFERENCES noise.safety_directives(directive_id),
    active_from timestamptz NOT NULL
);

CREATE TABLE noise.idempotency_keys (
    device_pk bigint NOT NULL REFERENCES noise.devices(device_pk),
    endpoint text NOT NULL CHECK (endpoint LIKE '/v1/%'),
    idempotency_key_hash bytea NOT NULL
        CHECK (octet_length(idempotency_key_hash) = 32),
    request_fingerprint bytea NOT NULL
        CHECK (octet_length(request_fingerprint) = 32),
    response_status smallint NOT NULL CHECK (response_status BETWEEN 200 AND 599),
    response_body bytea NOT NULL DEFAULT ''::bytea,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (device_pk, endpoint, idempotency_key_hash),
    CHECK (expires_at > created_at),
    CHECK (octet_length(response_body) <= 65536)
);

CREATE INDEX idempotency_keys_expiry_idx
    ON noise.idempotency_keys (expires_at);

CREATE TABLE noise.outbox_events (
    outbox_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic text NOT NULL CHECK (topic ~ '^[a-z0-9_.-]+$'),
    aggregate_kind text NOT NULL CHECK (aggregate_kind ~ '^[a-z0-9_]+$'),
    aggregate_id bytea NOT NULL CHECK (octet_length(aggregate_id) > 0),
    payload bytea NOT NULL CHECK (octet_length(payload) > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    published_at timestamptz,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_error_code text
);

CREATE INDEX outbox_events_ready_idx
    ON noise.outbox_events (next_attempt_at, outbox_id)
    WHERE published_at IS NULL;

CREATE TABLE noise.durable_jobs (
    job_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_kind text NOT NULL CHECK (job_kind ~ '^[a-z0-9_.-]+$'),
    deduplication_key bytea NOT NULL UNIQUE
        CHECK (octet_length(deduplication_key) = 32),
    payload bytea NOT NULL CHECK (octet_length(payload) > 0),
    state text NOT NULL DEFAULT 'ready'
        CHECK (state IN ('ready', 'running', 'succeeded', 'failed', 'cancelled')),
    priority smallint NOT NULL DEFAULT 0 CHECK (priority BETWEEN -100 AND 100),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 10 CHECK (max_attempts > 0),
    run_after timestamptz NOT NULL DEFAULT clock_timestamp(),
    lease_owner text,
    lease_expires_at timestamptz,
    last_error_code text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    CHECK ((state = 'running' AND lease_owner IS NOT NULL
            AND lease_expires_at IS NOT NULL)
        OR (state <> 'running' AND lease_owner IS NULL
            AND lease_expires_at IS NULL)),
    CHECK ((state IN ('succeeded', 'failed', 'cancelled')
            AND completed_at IS NOT NULL)
        OR (state IN ('ready', 'running') AND completed_at IS NULL))
);

CREATE INDEX durable_jobs_ready_idx
    ON noise.durable_jobs (priority DESC, run_after, job_id)
    WHERE state = 'ready';

INSERT INTO noise.schema_migrations (version, name)
VALUES (1, 'canonical_schema');

COMMIT;
