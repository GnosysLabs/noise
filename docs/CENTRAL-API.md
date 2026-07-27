# noise central API

Status: authentication service implemented and disposable-database validated;
not deployed

Updated: 2026-07-27

Implementation: `crates/noise-central`

## Current surface

The first runnable central-service layer implements:

| Method and path | Purpose |
| --- | --- |
| `GET /health` | Verify database reachability and canonical schema version |
| `POST /v1/auth/challenges/registration` | Issue a two-minute account-bound installation-registration challenge |
| `POST /v1/devices/register` | Verify an identity-signed registration and create or deliberately rotate an installation binding |
| `POST /v1/auth/challenges/session` | Issue a two-minute challenge for an active installation |
| `POST /v1/auth/sessions` | Verify the same installation's proof and return a one-hour opaque bearer token |
| `DELETE /v1/auth/sessions/current` | Revoke the current bearer token |
| `POST /v1/devices/{installation_id}/revoke` | Verify an account-signed revocation and invalidate every installation token |

The `installation_id` path value is the canonical unpadded base64 value and
must be URL-encoded by a client when it contains reserved URL characters.

This service does not change the user-facing sign-in flow. Session challenges
and renewal are background operations performed by the same installation.

## Transactional guarantees

- Challenge consumption and registration/session creation commit in one
  PostgreSQL transaction.
- A challenge is account-, purpose-, and installation-bound, expires after two
  minutes, and can be consumed once.
- Registration retries with the exact already-accepted signed object are
  idempotent.
- A higher identity-signed registration version can rotate an active
  installation key and atomically revoke its previous sessions.
- A remotely revoked installation cannot register over the same binding or
  request another session challenge.
- Revocation is sequence-checked, idempotent for the same signed record, and
  atomically invalidates every active session for the installation.
- Client wall-clock correctness is not an authentication dependency. Challenge
  expiry uses the database clock, and revocation replay protection uses its
  signed monotonic sequence.

## Stored secrets

Challenge IDs and nonces contain 32 random bytes. PostgreSQL stores the
challenge ID and a BLAKE3 hash of the nonce; it does not store the raw nonce.

Access tokens contain 32 random bytes and are returned once. PostgreSQL stores
only:

`BLAKE3_KEYED(NOISE_TOKEN_HASH_KEY, raw_access_token)`

`NOISE_TOKEN_HASH_KEY` must contain 32 random bytes encoded as canonical
unpadded base64. It is a server secret, never a client setting, database value,
or committed file.

Request bodies, passwords, vault keys, identity secrets, installation private
keys, raw challenge nonces, and raw bearer tokens are not logged.

## Runtime configuration

| Environment variable | Required | Initial value or rule |
| --- | --- | --- |
| `NOISE_CENTRAL_LISTEN` | no | `127.0.0.1:4302`; non-loopback listeners are rejected |
| `NOISE_DATABASE_HOST` | no | `127.0.0.1` |
| `NOISE_DATABASE_PORT` | no | `5432` |
| `NOISE_DATABASE_NAME` | no | `noise` |
| `NOISE_DATABASE_USER` | no | `noise_app` |
| `NOISE_DATABASE_PASSWORD` | yes | Protected server environment only |
| `NOISE_DATABASE_POOL_SIZE` | no | `8`, accepted range `1..=16` |
| `NOISE_TOKEN_HASH_KEY` | yes | Independent 32-byte random secret |
| `NOISE_ALLOWED_ORIGIN` | no | Exact HTTPS origin, initially `https://app.makenoise.chat` when web traffic begins |

The production PostgreSQL role has a 20-connection limit. The service's
maximum accepted pool size leaves connections available for migrations,
maintenance, and recovery.

At startup the service requires canonical schema migration version 1. It does
not apply migrations automatically.

## HTTP boundary

- The server refuses to listen publicly and must remain behind nginx/TLS.
- Request bodies are limited to 64 KiB.
- Browser CORS is disabled unless one exact HTTPS origin is configured.
- Error responses contain stable codes, not database or cryptographic details.
- Registration and session challenge issuance are bounded to five concurrently
  active challenges per pseudonymous account/installation.
- nginx or another edge must add source-based abuse limits before the
  unauthenticated registration-challenge endpoint is public.

## Validation evidence

On 2026-07-27 the Linux service and canonical migration were exercised against
a disposable PostgreSQL database on Cyphers VPS. The test covered:

1. startup schema verification and health;
2. identity-authorized installation registration;
3. idempotent registration replay;
4. same-installation session creation;
5. rejection of one-time challenge reuse;
6. current-session logout;
7. a new invisible session;
8. account-signed installation revocation;
9. revocation idempotency;
10. rejection of the revoked bearer token and new session challenges; and
11. confirmation that raw bearer tokens and raw challenge nonces were not
    stored.

The disposable database and source/build directories were removed afterward.
The production `noise` schema remained empty.

## Not implemented yet

The service is not production-ready and has no public nginx route. Remaining
central API layers include:

- encrypted account-vault compare-and-swap;
- group and membership authorization;
- canonical encrypted event publication and pagination;
- MLS control records;
- realtime WebSocket catch-up;
- media upload/download capabilities and R2 runtime credentials;
- safety restriction enforcement;
- transactional outbox workers and cleanup jobs;
- production migrations, backups, systemd, nginx, monitoring, and rollback;
  and
- client transport integration.
