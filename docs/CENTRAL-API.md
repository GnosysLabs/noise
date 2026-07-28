# noise central API

Status: authentication, encrypted account-vault, canonical group/direct-event,
and MLS control service implemented; not deployed

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
| `POST /v1/direct-events` | Verify and canonically order one receiver-addressed encrypted direct event |
| `GET /v1/direct-events` | Fetch one authenticated participant's visible direct-thread events after a cursor |
| `GET /v1/direct-events/latest` | Fetch the latest visible page for one authenticated direct-thread participant |
| `POST /v2/mls/genesis` | Verify and establish one founder-signed MLS epoch-zero control record |
| `POST /v2/mls/epochs` | Verify and append one authorized MLS epoch transition to the current head |
| `POST /v2/mls/external-joins` | Verify a current invitation and append the frequency holder's self-authored external commit |
| `POST /v2/mls/external-join-packages` | Store a current member's signed, frequency-encrypted continuity package for the current head |
| `GET /v2/mls/external-join-packages/by-invite/{locator}` | Fetch the current continuity package through the active invitation |
| `GET /v2/mls/groups/{group_id}/external-join-package` | Let a current member check whether the current head has a continuity package |
| `POST /v2/mls/join-requests` | Store an authenticated account's signed group-scoped KeyPackage request |
| `POST /v2/mls/removal-requests` | Store a current member's signed self-leave or founder-reviewed ban request |
| `GET /v2/mls/groups/{group_id}` | Fetch and reverify the complete canonical MLS control log |
| `GET /v2/mls/groups/{group_id}/join-requests` | Fetch signed join requests as an active member |
| `GET /v2/mls/groups/{group_id}/removal-requests` | Fetch signed removal requests as an active member |

The `installation_id` path value is the canonical unpadded base64 value and
must be URL-encoded by a client when it contains reserved URL characters.

This service does not change the user-facing sign-in flow. Session challenges
and renewal are background operations performed by the same installation.

## Canonical direct threads

The direct-event publication body contains `recipient_public_key` and `event`.
The event is the existing signed, encrypted receiver-mailbox copy. The server
derives the expected receiver mailbox from the public key and rejects the
request unless the signed event's `group_id` matches it. This binds the
otherwise encrypted routing claim without exposing message text, profiles,
attachments, block state, or thread-deletion state.

The server derives the deterministic two-participant `direct_scope_id`, creates
one canonical thread and stream for that account pair, and stores only the
receiver-addressed copy. Both participants fetch the same canonical stream and
can decrypt that copy with their existing pairwise secret. The sender-mailbox
duplicate used by the legacy relay transport is not stored a second time.

Reads supply `peer_public_key` as a URL-encoded query parameter and require a
valid bearer session. A lookup returns a stream only when the authenticated
account and the named peer are the two accounts bound to the derived scope.
Unknown accounts, nonexistent threads, and unrelated account pairs all produce
an empty page rather than exposing thread existence.

Existing pairwise block changes remain encrypted direct events and are enforced
by clients, as they are today. The server cannot infer a private block from
ciphertext. Safety-wide account restrictions are server-visible and prevent
the restricted account from authenticating; an actively restricted or deleted
recipient cannot receive a newly accepted direct event. Hidden or restricted
direct events are omitted from both participants' pages while the returned
high-water cursor still advances past the moderated envelope.

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
- Every signed vault locator and identity are bound to one authenticated
  account. An account may retain multiple recovery-locator aliases, each with
  its own signed revision chain.
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
- Direct publication requires the authenticated author and an active,
  unrestricted recipient; verifies that the signed receiver-mailbox ID matches
  the claimed recipient; and binds the event to exactly one deterministic
  two-account scope.
- Direct reads require either bound participant, return one canonical encrypted
  receiver copy to both, and apply the same event hiding/restriction filter and
  safe high-water cursor behavior as group reads.
- Exact direct-event retries return the original cursor, including retries that
  race while the direct thread is being created. A conflicting event ID or
  author-sequence claim is rejected.
- MLS genesis creates or binds the canonical group and founder, records epoch
  zero, and materializes the founder membership in one transaction.
- Epoch writes lock the group control head, require an exact parent record and
  epoch, verify that the author belonged to the parent snapshot, and permit
  removals only from the founder. An exact accepted retry is idempotent.
- Every accepted epoch stores its complete signed account-membership snapshot
  and atomically opens or closes current membership intervals before the
  transaction commits.
- Join requests remain inert recovery records. A current invitation authorizes
  its holder to append an external commit that only adds that holder. Only the
  founder may author an epoch that removes an account.
- Each accepted MLS object creates a durable outbox record in the same
  transaction. Control-log and request reads reverify stored signed objects
  before returning them.

## Stored secrets

Challenge IDs and nonces contain 32 random bytes. PostgreSQL stores the
challenge ID and a BLAKE3 hash of the nonce; it does not store the raw nonce.

Access tokens contain 32 random bytes and are returned once. PostgreSQL stores
only:

`BLAKE3_KEYED(NOISE_TOKEN_HASH_KEY, raw_access_token)`

`NOISE_TOKEN_HASH_KEY` must contain 32 random bytes encoded as canonical
unpadded base64. It is a server secret, never a client setting, database value,
or committed file.

Account-vault, event, and MLS records contain signed ciphertext or public
control envelopes. The service stores the pseudonymous account keys and
membership snapshots already present in the signed MLS protocol, but no MLS
private state, archive key, account-vault key, message plaintext, media
plaintext, group name, or profile plaintext.

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
| `NOISE_KLIPY_API_KEY` | no | Protected server-side key enabling authenticated GIF, sticker, and clip search |

The production PostgreSQL role has a 20-connection limit. The service's
maximum accepted pool size leaves connections available for migrations,
maintenance, and recovery.

At startup the service requires canonical schema migration version 2. It does
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
6. a second authenticated account, founder-signed MLS genesis, signed join
   request, real OpenMLS admission commit, epoch-one membership
   materialization, and epoch-encrypted event publication;
7. signed self-removal request, founder-authored removal commit, epoch-two
   membership materialization, rejection of the removed member's next event,
   retained control-log access, and successful founder publication;
8. a third authenticated account, receiver-bound direct-event publication,
   exact retry, rejection of a sender-mailbox event misrouted as a receiver
   copy, two-party pagination and local decryption in both directions;
9. moderation hiding of a direct event with safe cursor advancement, plus the
   one-thread/one-stream database invariants;
10. current-session logout and a new invisible session;
11. account-signed installation revocation and idempotent replay;
12. rejection of the revoked bearer token and new session challenges; and
13. confirmation that raw bearer tokens and raw challenge nonces were not
   stored.

The disposable database and source/build directories were removed afterward.
The production `noise` schema remained empty.

## Not implemented yet

The service is not production-ready and has no public nginx route. Remaining
central API layers include:

- realtime WebSocket catch-up;
- media upload/download capabilities and R2 runtime credentials;
- safety directive ingestion and restriction maintenance;
- transactional outbox workers and cleanup jobs;
- production migrations, backups, systemd, nginx, monitoring, and rollback;
  and
- client transport integration.
