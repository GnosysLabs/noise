# noise canonical PostgreSQL schema

Status: initial schema contract consumed by the authentication,
encrypted-account-vault, canonical group/direct-event, and MLS control service
layers

Updated: 2026-07-27

Migrations:

- `deploy/central/migrations/0001_canonical_schema.sql`
- `deploy/central/migrations/0002_recovery_aliases.sql`

## Purpose

PostgreSQL becomes the authoritative record for accepted encrypted objects,
canonical ordering, authorization metadata, sessions, media state, safety
enforcement, and durable work. It does not become a plaintext conversation
database.

The schema stores decoded binary protocol fields as `bytea` and preserves exact
signed wire records as bytes where the service needs to replay or reverify
them. Rust `u64` values use `numeric(20,0)` because PostgreSQL `bigint` is
signed.

## Important boundaries

- Identity private keys, account passwords, vault keys, MLS secrets, message
  plaintext, media plaintext, profile plaintext, group names, and device names
  are not stored.
- Application code verifies signatures, hashes, protocol structure, and MLS
  authorization before inserting accepted records.
- Database constraints protect shape, uniqueness, parentage, and lifecycle
  invariants; they do not replace cryptographic verification.
- Accounts, devices, groups, events, and signed control records are never
  hard-deleted during ordinary operation.
- Raw access tokens, raw challenge nonces, raw push tokens, presigned R2
  capabilities, and safety-report contents are not stored in these tables.

## Canonical cursor

`cursor_clock` has one row. An event transaction assigns a cursor with:

```sql
UPDATE noise.cursor_clock
SET last_cursor = last_cursor + 1
WHERE singleton
RETURNING last_cursor;
```

The row lock remains held until commit. A competing event transaction waits,
so a higher cursor cannot commit before an uncommitted lower cursor. A rolled
back transaction does not leave a permanent gap. WebSocket notifications are
published from the transactional outbox only after the event commit.

## Minimum server-visible membership

The service needs a pseudonymous active-membership set to authorize event
publication, history fetches, WebSocket delivery, media capabilities, and push
fan-out.

Authoritative membership snapshots come from verified MLS genesis and epoch
records, whose signed public fields already contain account public keys. The
service materializes intervals in `group_memberships`; it does not decrypt
ordinary group events to discover membership.

The founder is visible from the verified MLS genesis. Existing moderator
changes are encrypted `ModeratorSet` events and therefore cannot be inferred by
the service. Ordinary group moderation remains client/founder enforced during
compatibility. If server-side moderator authorization is later required, it
needs a new founder-signed, server-visible role record with a protocol version
and migration plan; the database has a `signed_role` source kind for that
future record, but this migration does not invent its wire format.

## Events and streams

`events` stores the existing signed encrypted envelope plus one global
canonical cursor. The service preserves:

- event ID;
- protocol group/scope ID;
- author account and author sequence;
- client creation time;
- encryption version, MLS epoch, and optional stream locator;
- nonce, ciphertext, and signature; and
- exact accepted wire bytes.

The event ID is globally unique. Author-sequence claims are unique within the
protocol scope. Before importing production history, the migration verifier
must prove that the signed union satisfies this invariant. Any equivocation is
recorded and reviewed; one relay is never silently preferred.

Topics are streams under a group. Direct conversations have a deterministic
protocol scope, canonical two-account binding, and one stream. The central
service stores one existing receiver-mailbox encrypted envelope per logical
direct event; both bound participants read that envelope rather than retaining
the legacy relay transport's second sender-mailbox copy. Cursors describe
transport order, not permission to decrypt or display an invalid event.

## Account vaults

The current verified encrypted account-vault revision is retained in
`account_vault_versions`. `account_vault_heads` selects that accepted revision
for a locator. Updating the head uses compare-and-swap in one transaction, and
the database removes the superseded full-vault snapshot after the head moves.
This keeps vault storage proportional to active recovery locators instead of
the number of account mutations.

An account may own multiple identity-signed recovery locators. Each locator
retains its own current revision and head; all of those locator aliases resolve
to the same canonical account.

Deleted vaults retain their signed tombstone and contain no nonce or
ciphertext. The importer verifies both relays and selects the highest valid
signed revision; it does not make either relay globally authoritative.

## Media

`media_objects` and `media_blocks` contain opaque object IDs, ciphertext
lengths and hashes, generated private R2 keys, and lifecycle state. No key
contains an account ID, group ID, filename, username, or plaintext MIME type.

Legacy provider/shard tables preserve every old signed lookup identity while
allowing duplicate aliases to point at one normalized private R2 object.
Upgraded clients address that object canonically; only the compatibility
service reproduces legacy `NSB2` or exact JSON shard responses. Deleting an
object is a state transition plus durable job, never an uncoordinated row
deletion.

## Safety

One reviewer decision can issue multiple signed directives, so hiding an event
and blocking its author are independent actions linked by the same
`action_set_id`. Active restriction tables provide fast read/write enforcement
while `safety_directives` remains the immutable signed audit record.

Report contents remain in the separate sealed safety system. The public
service receives only a verified signed directive and the minimum target
identifier needed to enforce it.

## Durable jobs and idempotency

State changes and their `outbox_events`/`durable_jobs` rows commit together.
Workers claim jobs with `FOR UPDATE SKIP LOCKED`, bounded batches, leases, and
unique deduplication keys.

Mutation retries use `idempotency_keys`. The database stores a request
fingerprint and bounded replay result, not a plaintext request body.

## Deployment

The migrations are applied in version order by the migration process, not
automatically at service startup. Production application traffic remains on
the relays until:

1. the migration has been applied to an isolated PostgreSQL database;
2. the schema and constraints have been inspected;
3. production backups and rollback commands exist;
4. the central service can consume the schema; and
5. the compatibility importer verifies the production object union.

The service now consumes the account, device, session, account-vault, group,
direct-thread, membership, MLS control, stream, event, restriction, cursor, and
outbox portions of this contract in isolated validation. Media, push,
safety-directive ingestion, and workers remain to be implemented.

The empty production `noise` database is intentionally left unchanged by these
design and service-validation steps.
