# noise central API

Status: authentication, encrypted account-vault, and canonical group-event
service implemented; not deployed

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
| `GET /v1/account-vaults/{locator}` | Bootstrap-fetch the current signed encrypted vault and revision ETag by its opaque locator |
| `PUT /v1/account-vaults/{locator}` | Compare-and-swap the authenticated account's next signed encrypted vault revision |
| `POST /v1/events` | Verify and canonically order a signed encrypted group or topic event |
| `GET /v1/groups/{group_id}/events` | Fetch visible events after a canonical cursor |
| `GET /v1/groups/{group_id}/events/latest` | Fetch the latest visible canonical event page |

The `installation_id` path value is the canonical unpadded base64 value and
must be URL-encoded by a client when it contains reserved URL characters.

This service does not change the user-facing sign-in flow. Session challenges
and renewal are background operations performed by the same installation.

## Account restore bootstrap

A new installation starts with only the user's noise ID and password. Those
values derive a 256-bit account locator and vault key locally. The installation
must download the encrypted vault before it can recover the identity key needed
to register itself, so vault `GET` cannot require an already-existing bearer
session.

The locator is the capability for this read, as in the existing relay protocol.
The response is still identity-signed ciphertext; the server never receives
the password or vault key, and only the client can decrypt it. Vault creation,
updates, and deletion remain bearer-authenticated and identity-signed. The
response is marked `Cache-Control: no-store`. The public edge must rate-limit
bootstrap reads and return the same compact not-found response without
revealing account metadata.

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
- Vault writes require `If-Match`, advance exactly one signed revision, retain
  every accepted encrypted version, and update the head atomically. Exact
  retries are idempotent.
- A signed vault locator and identity are bound to one authenticated account.
  A signed tombstone marks the account deleted and revokes all of its sessions
  in the same transaction.
- Event publication verifies the envelope signature and authenticated author,
  requires active group membership, enforces active account/group/event safety
  restrictions, assigns one commit-ordered canonical cursor, and creates its
  outbox record in the same transaction.
- Exact event retries return the original cursor. A different event claiming
  the same protocol scope, author, and sequence is rejected.
- Event reads require active membership and omit hidden or restricted events.
  The returned high-water cursor lets a client advance safely past moderated
  gaps without seeing the hidden envelopes.

## Stored secrets

Challenge IDs and nonces contain 32 random bytes. PostgreSQL stores the
challenge ID and a BLAKE3 hash of the nonce; it does not store the raw nonce.

Access tokens contain 32 random bytes and are returned once. PostgreSQL stores
only:

`BLAKE3_KEYED(NOISE_TOKEN_HASH_KEY, raw_access_token)`

`NOISE_TOKEN_HASH_KEY` must contain 32 random bytes encoded as canonical
unpadded base64. It is a server secret, never a client setting, database value,
or committed file.

Account-vault and event records contain signed ciphertext envelopes. The
service stores no account-vault key, message plaintext, media plaintext, group
name, or profile plaintext.

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
- Request bodies are limited to 3,000,000 bytes so the existing encrypted
  account-vault and event envelope limits fit without accepting unbounded
  uploads. Media bytes use a separate capability flow and never pass through
  these JSON routes.
- Browser CORS is disabled unless one exact HTTPS origin is configured.
- Browser CORS permits `GET`, `POST`, `PUT`, and `DELETE`, accepts
  `Authorization`, `Content-Type`, and `If-Match`, and exposes only the vault
  `ETag` response header.
- Error responses contain stable codes, not database or cryptographic details.
- Registration and session challenge issuance are bounded to five concurrently
  active challenges per pseudonymous account/installation.
- nginx or another edge must add source-based abuse limits before the
  unauthenticated registration-challenge and encrypted-vault bootstrap
  endpoints are public.

## Validation evidence

On 2026-07-27 the Linux service and canonical migration were exercised against
a disposable PostgreSQL database on Cyphers VPS. The test covered:

1. startup schema verification and health;
2. identity-authorized installation registration and idempotent replay;
3. same-installation session creation and rejection of challenge reuse;
4. encrypted vault create, exact retry, unauthenticated bootstrap
   fetch/local-decrypt, stale compare-and-swap rejection, and next-revision
   update;
5. active-membership group-event publication, exact retry, sequence-conflict
   rejection, canonical pagination, latest-page fetch, and outbox creation;
6. current-session logout and a new invisible session;
7. account-signed installation revocation and idempotent replay;
8. rejection of the revoked bearer token and new session challenges; and
9. confirmation that raw bearer tokens and raw challenge nonces were not
   stored.

The disposable database and source/build directories were removed afterward.
The production `noise` schema remained empty.

## Not implemented yet

The service is not production-ready and has no public nginx route. Remaining
central API layers include:

- MLS control records;
- canonical direct-thread authorization and encrypted direct events;
- realtime WebSocket catch-up;
- media upload/download capabilities and R2 runtime credentials;
- safety directive ingestion and restriction maintenance;
- transactional outbox workers and cleanup jobs;
- production migrations, backups, systemd, nginx, monitoring, and rollback;
  and
- client transport integration.
