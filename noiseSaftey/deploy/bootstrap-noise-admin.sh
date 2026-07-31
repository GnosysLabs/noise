#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "bootstrap-noise-admin.sh must run as root" >&2
    exit 1
fi

service_user="noise-admin"
database_name="noise"
database_role="noise_admin"
config_directory="/etc/noise-admin"
environment_file="${config_directory}/environment"

if ! command -v psql >/dev/null 2>&1; then
    echo "PostgreSQL is not installed" >&2
    exit 1
fi
if ! systemctl is-active --quiet postgresql; then
    echo "PostgreSQL is not active" >&2
    exit 1
fi

if ! id "${service_user}" >/dev/null 2>&1; then
    useradd \
        --system \
        --user-group \
        --home-dir /var/lib/noise-admin \
        --shell /usr/sbin/nologin \
        "${service_user}"
fi
install -d -o "${service_user}" -g "${service_user}" -m 0750 /var/lib/noise-admin
install -d -o root -g "${service_user}" -m 0750 "${config_directory}"

role_exists="$(
    runuser -u postgres -- psql -X --no-align --tuples-only \
        -c "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${database_role}')"
)"

if [[ "${role_exists}" == "f" ]]; then
    if [[ -e "${environment_file}" ]]; then
        echo "refusing to reuse ${environment_file} without its PostgreSQL role" >&2
        exit 1
    fi
    database_password="$(openssl rand -hex 32)"
    {
        printf "\\set role_password '%s'\n" "${database_password}"
        cat <<'SQL'
CREATE ROLE noise_admin
  WITH LOGIN
  PASSWORD :'role_password'
  NOSUPERUSER
  NOCREATEDB
  NOCREATEROLE
  NOINHERIT
  NOREPLICATION
  NOBYPASSRLS
  CONNECTION LIMIT 4;
SQL
    } | runuser -u postgres -- psql -X -v ON_ERROR_STOP=1

    install -o root -g "${service_user}" -m 0640 /dev/null "${environment_file}"
    {
        printf 'NOISE_ADMIN_DATABASE_HOST=127.0.0.1\n'
        printf 'NOISE_ADMIN_DATABASE_PORT=5432\n'
        printf 'NOISE_ADMIN_DATABASE_NAME=%s\n' "${database_name}"
        printf 'NOISE_ADMIN_DATABASE_USER=%s\n' "${database_role}"
        printf 'NOISE_ADMIN_DATABASE_PASSWORD=%s\n' "${database_password}"
    } >"${environment_file}"
    unset database_password
elif [[ ! -f "${environment_file}" ]]; then
    echo "${database_role} exists but ${environment_file} is missing" >&2
    exit 1
fi

runuser -u postgres -- psql -X -v ON_ERROR_STOP=1 --dbname="${database_name}" <<'SQL'
BEGIN;
REVOKE ALL ON DATABASE noise FROM noise_admin;
GRANT CONNECT ON DATABASE noise TO noise_admin;
REVOKE ALL ON SCHEMA noise FROM noise_admin;
REVOKE ALL ON ALL TABLES IN SCHEMA noise FROM noise_admin;

CREATE SCHEMA IF NOT EXISTS noise_admin AUTHORIZATION postgres;
REVOKE ALL ON SCHEMA noise_admin FROM PUBLIC;
REVOKE CREATE ON SCHEMA noise_admin FROM noise_admin;

CREATE OR REPLACE VIEW noise_admin.operational_totals
WITH (security_barrier = true) AS
SELECT
    (SELECT version FROM noise.schema_migrations ORDER BY version DESC LIMIT 1) AS schema_version,
    pg_database_size(current_database())::bigint AS database_bytes,
    (SELECT COUNT(*) FROM noise.accounts)::bigint AS total_accounts,
    (SELECT COUNT(*) FROM noise.accounts WHERE status = 'active')::bigint AS active_accounts,
    (SELECT COUNT(*) FROM noise.accounts WHERE created_at >= now() - interval '24 hours')::bigint AS new_accounts_24h,
    (SELECT COUNT(*) FROM noise.accounts WHERE created_at >= now() - interval '7 days')::bigint AS new_accounts_7d,
    (SELECT COUNT(DISTINCT account_id) FROM noise.devices WHERE revoked_at IS NULL AND last_seen_at >= now() - interval '24 hours')::bigint AS active_accounts_24h,
    (SELECT COUNT(DISTINCT account_id) FROM noise.devices WHERE revoked_at IS NULL AND last_seen_at >= now() - interval '7 days')::bigint AS active_accounts_7d,
    (SELECT COUNT(DISTINCT account_id) FROM noise.devices WHERE revoked_at IS NULL AND last_seen_at >= now() - interval '30 days')::bigint AS active_accounts_30d,
    (SELECT COUNT(*) FROM noise.devices WHERE revoked_at IS NULL)::bigint AS active_devices,
    (SELECT COUNT(*) FROM noise.groups WHERE lifecycle_state = 'active')::bigint AS active_groups,
    (SELECT COUNT(*) FROM noise.groups WHERE created_at >= now() - interval '7 days')::bigint AS new_groups_7d,
    (SELECT COUNT(*) FROM noise.group_memberships WHERE active_until_cursor IS NULL)::bigint AS memberships,
    (SELECT COUNT(*) FROM noise.events)::bigint AS total_events,
    (SELECT COUNT(*) FROM noise.events WHERE accepted_at >= now() - interval '24 hours')::bigint AS events_24h,
    (SELECT COUNT(*) FROM noise.events WHERE accepted_at >= now() - interval '7 days')::bigint AS events_7d,
    (SELECT COUNT(*) FROM noise.events WHERE scope_kind = 'group' AND accepted_at >= now() - interval '24 hours')::bigint AS group_events_24h,
    (SELECT COUNT(*) FROM noise.events WHERE scope_kind = 'direct' AND accepted_at >= now() - interval '24 hours')::bigint AS direct_events_24h,
    (SELECT COUNT(DISTINCT author_account_id) FROM noise.events WHERE accepted_at >= now() - interval '24 hours')::bigint AS authors_24h,
    (SELECT COUNT(*) FROM noise.media_objects WHERE state = 'available')::bigint AS available_media,
    (SELECT COALESCE(SUM(ciphertext_length), 0)::bigint FROM noise.media_objects WHERE state IN ('available', 'deleting')) AS media_bytes,
    (SELECT COUNT(*) FROM noise.media_objects WHERE created_at >= now() - interval '7 days')::bigint AS media_7d,
    (SELECT COUNT(*) FROM noise.sessions WHERE revoked_at IS NULL AND expires_at > now())::bigint AS active_sessions,
    (SELECT COUNT(*) FROM noise.push_subscriptions WHERE revoked_at IS NULL)::bigint AS active_push_subscriptions,
    (SELECT COUNT(*) FROM noise.durable_jobs WHERE state = 'ready')::bigint AS ready_jobs,
    (SELECT COUNT(*) FROM noise.durable_jobs WHERE state = 'running')::bigint AS running_jobs,
    (SELECT COUNT(*) FROM noise.durable_jobs WHERE state = 'failed')::bigint AS failed_jobs,
    (SELECT COUNT(*) FROM noise.outbox_events WHERE published_at IS NULL)::bigint AS pending_outbox,
    (SELECT COALESCE(EXTRACT(EPOCH FROM (now() - MIN(created_at)))::bigint, 0) FROM noise.outbox_events WHERE published_at IS NULL) AS oldest_outbox_seconds,
    (
        (SELECT COUNT(*) FROM noise.event_restrictions WHERE expires_at IS NULL OR expires_at > now()) +
        (SELECT COUNT(*) FROM noise.group_restrictions WHERE expires_at IS NULL OR expires_at > now()) +
        (SELECT COUNT(*) FROM noise.account_restrictions WHERE expires_at IS NULL OR expires_at > now()) +
        (SELECT COUNT(*) FROM noise.media_restrictions)
    )::bigint AS active_restrictions;

CREATE OR REPLACE VIEW noise_admin.daily_usage
WITH (security_barrier = true) AS
WITH days AS (
    SELECT generate_series(current_date - 13, current_date, interval '1 day') AS day
)
SELECT
    days.day::date AS day,
    to_char(days.day, 'Mon DD') AS label,
    COUNT(events.event_id)::bigint AS events,
    COUNT(DISTINCT events.author_account_id)::bigint AS authors
FROM days
LEFT JOIN noise.events AS events
    ON events.accepted_at >= days.day
   AND events.accepted_at < days.day + interval '1 day'
GROUP BY days.day;

CREATE OR REPLACE VIEW noise_admin.enforcement_audit
WITH (security_barrier = true) AS
SELECT action, target_kind, reason_code, issued_at
FROM noise.safety_directives;

GRANT USAGE ON SCHEMA noise_admin TO noise_admin;
GRANT SELECT ON
    noise_admin.operational_totals,
    noise_admin.daily_usage,
    noise_admin.enforcement_audit
TO noise_admin;
ALTER ROLE noise_admin IN DATABASE noise SET default_transaction_read_only = on;
ALTER ROLE noise_admin IN DATABASE noise SET statement_timeout = '5s';
ALTER ROLE noise_admin IN DATABASE noise SET search_path = noise_admin, public;
COMMIT;
SQL

set -a
# shellcheck disable=SC1090
source "${environment_file}"
set +a
PGPASSWORD="${NOISE_ADMIN_DATABASE_PASSWORD}" \
    psql \
        --host="${NOISE_ADMIN_DATABASE_HOST}" \
        --port="${NOISE_ADMIN_DATABASE_PORT}" \
        --username="${NOISE_ADMIN_DATABASE_USER}" \
        --dbname="${NOISE_ADMIN_DATABASE_NAME}" \
        -X \
        -v ON_ERROR_STOP=1 \
        -c "SELECT total_accounts FROM noise_admin.operational_totals" >/dev/null
if PGPASSWORD="${NOISE_ADMIN_DATABASE_PASSWORD}" \
    psql \
        --host="${NOISE_ADMIN_DATABASE_HOST}" \
        --port="${NOISE_ADMIN_DATABASE_PORT}" \
        --username="${NOISE_ADMIN_DATABASE_USER}" \
        --dbname="${NOISE_ADMIN_DATABASE_NAME}" \
        -X \
        -v ON_ERROR_STOP=1 \
        -c "SELECT account_id FROM noise.accounts LIMIT 1" >/dev/null 2>&1
then
    echo "noise_admin unexpectedly has raw production-table access" >&2
    exit 1
fi
unset PGPASSWORD NOISE_ADMIN_DATABASE_PASSWORD

echo "noise admin read-only database access provisioned"
