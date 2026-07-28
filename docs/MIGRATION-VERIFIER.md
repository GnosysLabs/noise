# noise migration verifier

Status: implemented, fixture-validated, and passed against isolated copies of
both complete production databases and every referenced encrypted media file;
durable backup and cutover capture still pending

Updated: 2026-07-27

Implementation: `crates/noise-migration-verifier`

## Purpose

The migration verifier proves what can safely enter the canonical PostgreSQL
database before an importer is allowed to write anything. It reads clean relay
SQLite snapshots and copied encrypted shard trees. It does not connect to
PostgreSQL, contact a relay, decrypt a vault, decrypt an event, modify a
snapshot, or import an object.

The JSON report is deliberately sanitized. It contains source labels, counts,
byte totals, and cryptographic digests, but no filesystem paths, account
locators, public keys, group IDs, event IDs, shard IDs, push tokens, message
contents, media contents, or signed payloads.

## Snapshot requirements

Each `--source` must be a clean standalone SQLite backup:

- the database must be a regular file;
- no matching `-wal` or `-shm` sidecar may exist;
- `PRAGMA integrity_check` must return `ok`; and
- the database bytes and length must remain unchanged throughout verification.

Each matching `--media-root` must contain the copied `shards/` directory from
that relay. Production verification must run against isolated backups, not the
live mutable relay directories.

The verifier opens SQLite with `SQLITE_OPEN_READ_ONLY`, enables connection-level
`query_only`, and hashes the snapshot before and after reading it.

## Usage

```bash
cargo run --release -p noise-migration-verifier -- \
  --source primary=/backups/noise-primary/relay.db \
  --media-root primary=/backups/noise-primary \
  --source secondary=/backups/noise-secondary/relay.db \
  --media-root secondary=/backups/noise-secondary \
  --primary-source primary \
  --output /backups/noise-migration-report.json
```

`--primary-source` selects the one relay whose existing push subscriptions and
delivery-deduplication rows will be eligible for import. It does not make that
relay authoritative for accounts, events, MLS records, invitations, deletions,
or media.

The report is written even when verification is blocked. A passing report exits
successfully; a blocked report exits nonzero after the JSON has been written.

## Signed-object verification

The verifier parses and cryptographically verifies:

- account vaults and signed account tombstones;
- encrypted events;
- invitations and invite rotations;
- group deletions;
- MLS genesis, epoch, join-request, and removal-request records;
- legacy encrypted blobs; and
- relay descriptors, evaluated at their signed issue time so an expired
  historical descriptor can still have its signature and lifetime verified.

Every relay row key must match its signed protocol identifier. Unknown object
kinds, invalid JSON, invalid signatures, malformed ciphertext envelopes, and
identifier mismatches are blockers.

Shared immutable IDs must contain byte-identical stored payloads. The verifier
imports neither side when the same immutable ID has conflicting bytes.

Account vaults are reconciled per locator. All valid revisions must retain the
same identity key, equal revisions must have identical signed bytes, and the
highest valid revision becomes the candidate head. One identity may have
multiple signed recovery locators. Those locators and their independent
revision histories are preserved as aliases for the same canonical account
and reported as a warning, not treated as conflicting accounts.

Complete MLS epoch chains are reconstructed and verified, rather than accepting
individually valid epoch signatures without their parent relationships.

## Event and direct-message reconciliation

For group and topic events, `(group ID, author key, author sequence)` normally
maps to one event ID. The production relays contain a small number of legacy
duplicate sequences whose complete candidate sets are byte-identically shared
by every source. These are reduced deterministically using the existing client
rebuild order: earliest `created_at_millis`, then lowest event ID. They are
reported as warnings. A candidate set that differs between sources is a
cross-source history conflict and blocks migration.

Legacy direct messages have two independently encrypted envelopes:

1. a receiver-mailbox copy; and
2. a sender-mailbox copy.

The verifier derives mailbox IDs from the verified account identity keys, groups
direct copies by signed author and author sequence, and identifies the
receiver-mailbox copy without decrypting it. That receiver copy becomes the one
canonical direct event; the sender copy is counted as collapsed legacy
transport duplication.

Migration is blocked when only a sender copy exists, when one author sequence
names multiple receiver mailboxes, or when duplicate sender/receiver copies
equivocate. A receiver copy without a sender copy is sufficient because both
participants can decrypt it.

## Encrypted media verification

For every `relay_shards` row, the verifier checks:

- canonical 32-byte hexadecimal shard, payload-hash, and delete-token-hash
  fields;
- the accepted ciphertext length range;
- the expected `shards/{prefix}/{shard_id}.bin` location;
- exact file length; and
- the BLAKE3 hash of the opaque ciphertext bytes.

It never decrypts those bytes. After integrity verification it also validates
that every unique payload is a complete identity-addressed encrypted blob.
Current `NSB2` storage bytes are accepted directly. The older exact JSON
encoding is parsed, cryptographically verified, round-tripped byte-for-byte for
legacy compatibility, and normalized to `NSB2` without opening its ciphertext.
An object ID resolving to different normalized bytes is a blocker.

Missing or corrupt referenced files block migration. Files without metadata
are reported as unclassified orphans and also block migration until they are
investigated. Live shard IDs and tombstones remain provider-scoped; each
legacy lookup maps to one verified canonical object so upgraded clients do not
need a separate shard-reading path.

## Digests

Object ID set digests reproduce the inventory convention:

1. sort the valid lowercase protocol IDs lexicographically;
2. concatenate their ASCII bytes with no delimiter (all protocol IDs are fixed
   width); and
3. compute SHA-256.

The schema digest is SHA-256 over length-prefixed, sorted
`(sqlite_master.type, name, sql)` values. Database digests are SHA-256 over the
exact snapshot bytes.

## Validation

Focused disposable SQLite/media fixtures cover:

- a valid two-relay union with a higher account revision;
- exact immutable deduplication;
- conversion of two legacy direct mailbox copies into one canonical event;
- duplicate encrypted media payload reconciliation;
- current and legacy-JSON encrypted-object normalization;
- rejection of validly hashed bytes that are not complete encrypted objects;
- conflicting bytes for one immutable ID;
- shared legacy events duplicating one author sequence;
- a cross-source author-sequence conflict; and
- an unclassified filesystem orphan.

The fixtures verify both passing and deliberately blocked reports.

The complete isolated production run passed with no blockers. It verified
6,233 provider-scoped references and 4,311,733,524 referenced bytes as 3,119
canonical encrypted objects. Of those, 214 use the exact older JSON encoding;
all normalize without decryption or conflict. Canonical `NSB2` storage totals
2,099,946,593 bytes. No referenced file was missing or corrupt.

The primary store's one unreferenced 1,048,686-byte file was preserved in a
separate quarantine copy and excluded from live import. Its July 25 timestamp
has no retained relay journal entry, so it is classified conservatively as an
unreferenced interrupted-or-uncommitted upload rather than guessed to be live
media.

The run also confirmed eight identities with signed recovery-locator aliases,
seven shared legacy group-sequence duplicates, and three valid legacy APNs
`sandbox` registrations. The next operational gate is a durable,
access-controlled backup and final cutover capture; the conversion contract
itself now passes against the complete current dataset.
