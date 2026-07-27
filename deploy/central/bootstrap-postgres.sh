#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
  echo "bootstrap-postgres.sh must run as root" >&2
  exit 1
fi

service_user="noise-central"
database_name="noise"
database_role="noise_app"
config_directory="/etc/noise-central"
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
    --home-dir /var/lib/noise-central \
    --shell /usr/sbin/nologin \
    "${service_user}"
fi

install -d -o root -g root -m 0755 /opt/noise-central
install -d -o "${service_user}" -g "${service_user}" -m 0750 /var/lib/noise-central
install -d -o root -g "${service_user}" -m 0750 "${config_directory}"

role_exists="$(
  runuser -u postgres -- psql -X --no-align --tuples-only \
    -c "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${database_role}')"
)"
database_exists="$(
  runuser -u postgres -- psql -X --no-align --tuples-only \
    -c "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${database_name}')"
)"

if [[ "${role_exists}" != "${database_exists}" ]]; then
  echo "refusing inconsistent PostgreSQL state: role/database existence differs" >&2
  exit 1
fi

if [[ "${role_exists}" == "t" ]]; then
  if [[ ! -f "${environment_file}" ]]; then
    echo "database already exists but ${environment_file} is missing" >&2
    exit 1
  fi
  echo "noise PostgreSQL foundation already exists"
  exit 0
fi

if [[ -e "${environment_file}" ]]; then
  echo "refusing to overwrite existing ${environment_file}" >&2
  exit 1
fi

database_password="$(openssl rand -hex 32)"
{
  printf "\\set role_password '%s'\n" "${database_password}"
  cat <<'SQL'
CREATE ROLE noise_app
  WITH LOGIN
  PASSWORD :'role_password'
  NOSUPERUSER
  NOCREATEDB
  NOCREATEROLE
  NOINHERIT
  NOREPLICATION
  NOBYPASSRLS
  CONNECTION LIMIT 20;
SQL
} | runuser -u postgres -- psql -X -v ON_ERROR_STOP=1

runuser -u postgres -- createdb \
  --owner="${database_role}" \
  --encoding=UTF8 \
  --template=template0 \
  "${database_name}"

runuser -u postgres -- psql -X -v ON_ERROR_STOP=1 --dbname="${database_name}" <<'SQL'
REVOKE ALL ON DATABASE noise FROM PUBLIC;
GRANT CONNECT ON DATABASE noise TO noise_app;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA noise AUTHORIZATION noise_app;
ALTER ROLE noise_app IN DATABASE noise SET search_path = noise, public;
SQL

install -o root -g "${service_user}" -m 0640 /dev/null "${environment_file}"
{
  printf 'NOISE_DATABASE_HOST=127.0.0.1\n'
  printf 'NOISE_DATABASE_PORT=5432\n'
  printf 'NOISE_DATABASE_NAME=%s\n' "${database_name}"
  printf 'NOISE_DATABASE_USER=%s\n' "${database_role}"
  printf 'NOISE_DATABASE_PASSWORD=%s\n' "${database_password}"
} >"${environment_file}"
unset database_password

set -a
# shellcheck disable=SC1090
source "${environment_file}"
set +a
PGPASSWORD="${NOISE_DATABASE_PASSWORD}" \
  psql \
    --host="${NOISE_DATABASE_HOST}" \
    --port="${NOISE_DATABASE_PORT}" \
    --username="${NOISE_DATABASE_USER}" \
    --dbname="${NOISE_DATABASE_NAME}" \
    -X \
    -v ON_ERROR_STOP=1 \
    -c "SELECT 1" >/dev/null
unset PGPASSWORD NOISE_DATABASE_PASSWORD

echo "noise PostgreSQL foundation provisioned"
