# noise centralization inventory

Status: read-only point-in-time inventory

Captured: 2026-07-27

Production mutations: none

This document records the first production inventory required by
`CENTRALIZED-ARCHITECTURE.md`. It contains aggregate counts and cryptographic
digests only. It does not contain account locators, identity keys, group IDs,
event IDs, shard IDs, message payloads, media bytes, credentials, or private
configuration.

This inventory is sufficient to define the merge rules for the central
importer. It does not complete Phase 0: durable immutable backups of the full
media stores, protected configuration snapshots, and an isolated restore drill
are still required before production migration.

## Sources

| Source | Public endpoint | Role |
| --- | --- | --- |
| Primary official relay | `https://noiserelay.gnosyslabs.xyz` | Relay, encrypted shard storage, and push records |
| Secondary official relay | `https://noiserelay.irisirc.chat` | Relay and encrypted shard storage |

Both public health endpoints and both loopback health endpoints were healthy
during the inventory. Both servers reported:

- noise relay `0.1.5`;
- protocol version `4`;
- the same relay binary SHA-256,
  `f462f15c2568f3d51ceed075b686f125ab418c51f978151d03762b76519263c7`;
- local-disk encrypted shard storage;
- privacy-gateway support with one mask target; and
- zero configured relay peers.

The released client source contains both official relay endpoints and pinned
OHTTP keys. The servers do not replicate through a peer connection. The
differences below therefore have to be reconciled by the migration rather than
waiting for server-to-server convergence.

## Snapshot method and evidence

The snapshots represent two close, but not atomic across servers, points in
time:

| Source | Snapshot time (UTC) | SQLite bytes | SHA-256 |
| --- | ---: | ---: | --- |
| Primary | 2026-07-27 20:19:08 | 45,191,168 | `4b4725e0856ade71d02ae2ccc2e7214ed2f1fe667136b1bef9d390d464e8f900` |
| Secondary | 2026-07-27 20:19:12 | 45,162,496 | `3bfe20d5e624f01307ae2d6aee7f55eb9cfe19d3e8bc4c53eab7bcfe60e9e18e` |

The running libSQL process held a lock that rejected SQLite's online backup
operation, including with a 30-second busy timeout. The non-disruptive fallback
was:

1. copy `relay.db` with its WAL sidecar into a unique temporary directory on
   the same server;
2. open only that isolated copy and require `PRAGMA integrity_check` to return
   `ok`;
3. use SQLite backup on the isolated copy to produce a clean snapshot;
4. hash the clean snapshot locally and independently re-run the integrity
   check; and
5. remove the remote temporary copies.

Both copies passed on the first attempt. Neither relay was stopped, restarted,
checkpointed, reconfigured, or written to by the inventory.

Writes continued normally during and after capture. At the snapshot, the
databases contained 1,459 and 1,458 event rows. Roughly three minutes later the
live processes reported 1,467 and 1,466 events. The one-event count difference
remained, while the exact set comparison below shows older differences on both
sides. A migration freeze or accepted-write journal is therefore required for
the final capture.

## Logical object inventory

| Object kind | Primary | Secondary | Shared IDs | Primary only | Secondary only | Shared payloads identical |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Account vaults | 31 | 31 | 31 | 0 | 0 | 30 |
| Group deletions | 10 | 10 | 10 | 0 | 0 | 10 |
| Events | 1,459 | 1,458 | 1,455 | 4 | 3 | 1,455 |
| Invitations | 4 | 4 | 4 | 0 | 0 | 4 |
| Invite rotations | 2 | 2 | 2 | 0 | 0 | 2 |
| MLS epochs | 29 | 29 | 29 | 0 | 0 | 29 |
| MLS geneses | 4 | 4 | 4 | 0 | 0 | 4 |
| MLS join requests | 55 | 55 | 55 | 0 | 0 | 55 |
| MLS removal requests | 0 | 0 | 0 | 0 | 0 | 0 |

All shared immutable records have byte-identical stored payloads. Only one
mutable account vault differs.

### Object-ID set digests

These SHA-256 values were computed over sorted object IDs without printing or
retaining the IDs in this document:

| Object kind | Primary digest | Secondary digest | Result |
| --- | --- | --- | --- |
| Account vaults | `50573f2b3ea073c6763dad8235c73a86c88f25e63b9e401a1ebacf3a939125b5` | same | Exact set match |
| Group deletions | `640ab63e5db4d480c1d87cde9601d4307bf66e1dfbc60fc505b4da9dce4cb43d` | same | Exact set match |
| Events | `d886caf8f30e12793ba11929fe6081c51c56b4e9588bec03b09eb3a360b3a1ad` | `b4e808902d145ac490106514c51311cf7c599036a14e45323a365706323c5b8b` | Different sets |
| Invitations | `81f4669762c12a907cd4e1f4ced294f4d151de47add2a4d983cc470a20276a2e` | same | Exact set match |
| Invite rotations | `e222a4da2b665860372906b5ab9b845285fe8c924d3feb3c9469363fd50c9db5` | same | Exact set match |
| MLS epochs | `0660e9163ad7654b5a0c436092df6207ef5641e56e10c12f848d8e47ba697f59` | same | Exact set match |
| MLS geneses | `466495be107afc9d98c218f21bb38c6d5f74560051d40d630d56c4bc25c85a1e` | same | Exact set match |
| MLS join requests | `c7e981161cf15f0f39cfae732b2ca6bee3588d429fde01abda8ad974f3bf6921` | same | Exact set match |

## Account reconciliation

Each relay stores 31 account vault rows:

- 24 active accounts;
- 7 signed deleted-account records;
- revisions from 1 through 18,384; and
- the exact same locator set.

One shared account differs:

| Primary revision | Secondary revision | Current public API check |
| ---: | ---: | --- |
| 216 | 217 | The mismatch still existed after the snapshots |

The importer must verify both signatures and select revision 217 for this
account. More generally, it must select the highest valid signed revision for
each locator. It must never pick a relay as globally authoritative for mutable
account vaults.

## Event reconciliation

The event union at snapshot time contains 1,462 unique verified event IDs:

- 1,455 present on both relays with identical payloads;
- 4 present only on the primary, spanning 3 groups and timestamps from
  2026-07-23 22:43:38 UTC through 2026-07-25 16:48:36 UTC; and
- 3 present only on the secondary, spanning 2 groups and timestamps from
  2026-07-23 22:29:11 UTC through 2026-07-23 22:33:32 UTC.

The differences predate the four-second gap between snapshots, so snapshot
timing does not explain them.

The importer must verify signatures and event structure, then import the union
deduplicated by event ID. Importing either relay alone would permanently omit
valid encrypted history.

## MLS, invitations, and deletions

Invitations, invite rotations, MLS join requests, MLS geneses, MLS epochs, and
group-deletion records match exactly by both object-ID set and payload.

There are no stored MLS removal requests and no queued shard deletions in
either snapshot.

Each relay also stores one relay-directory record. These are relay-local
descriptors and should not become canonical social data.

## Push state

| Table | Primary | Secondary |
| --- | ---: | ---: |
| Push subscriptions | 5 | 0 |
| Push delivery records | 46 | 0 |

The central service must treat the primary as the source for existing push
registrations and delivery-deduplication history. Tokens and mailbox identifiers
were not printed or included in this document.

## Encrypted media inventory

### Database metadata

| Metric | Primary | Secondary |
| --- | ---: | ---: |
| Referenced encrypted shards | 3,115 | 3,118 |
| Referenced ciphertext bytes | 2,153,944,067 | 2,157,789,457 |
| Shard tombstones | 528 | 528 |

Shard IDs are provider-specific:

- zero live shard IDs are shared across the two relays;
- zero tombstoned shard IDs are shared across the two relays;
- 3,114 live ciphertext payloads match by payload hash and byte length;
- 1 payload, totaling 1,048,686 bytes, exists only on the primary; and
- 4 payloads, totaling 4,894,076 bytes, exist only on the secondary.

The union is 3,119 distinct encrypted payloads totaling 2,158,838,143 bytes,
approximately 2.01 GiB. This is the minimum current R2 ciphertext capacity,
before database backups, temporary uploads, lifecycle headroom, and growth.

The compatibility importer must preserve `(legacy provider, shard ID)` as the
lookup identity. It may avoid storing duplicate ciphertext bytes internally
when the verified payload hash and length match, but both old provider URLs and
both old shard IDs must continue to resolve correctly.

### Filesystem reconciliation

| Source | `.bin` files | Metadata rows | Files without metadata | Metadata without files |
| --- | ---: | ---: | ---: | ---: |
| Primary | 3,116 | 3,115 | 1 | 0 |
| Secondary | 3,118 | 3,118 | 0 | 0 |

The primary has one unreferenced 1,048,686-byte file. It must not be imported as
live media merely because it exists on disk. Before final capture, the
migration verifier should classify it as an interrupted upload, failed
publication, or other orphan using metadata and logs without opening the
encrypted bytes. No orphan was found on the secondary.

## Required importer rules

The production evidence fixes the following rules:

1. Read and verify both relay databases; neither one is a complete source.
2. Import the signed union of immutable events and control records.
3. Deduplicate immutable records by their protocol ID and reject conflicting
   payloads for the same ID.
4. Select the highest valid signed account-vault revision per locator.
5. Preserve deleted-account and group-deletion records.
6. Namespace legacy shard IDs and tombstones by original relay provider.
7. Reconcile encrypted media by verified payload hash and byte length while
   preserving every legacy lookup path.
8. Import push state from the primary without logging tokens.
9. Exclude filesystem orphans unless a verified database reference exists.
10. Run a final frozen capture or journal every accepted write between capture
    and cutover.

## Phase 0 work still open

No production service change should begin until these gaps are closed:

- create durable, access-controlled, immutable backups of both clean database
  snapshots;
- capture both complete encrypted shard stores without opening media;
- capture service configuration into protected secret storage, excluding
  secrets from migration reports and Git;
- record DNS, TLS, systemd, firewall, and deployment recovery information;
- build an isolated restore environment and prove both database snapshots open;
- verify representative legacy shard retrieval from restored copies;
- classify the single primary filesystem orphan;
- build the signed-object migration verifier from these counts and merge rules;
  and
- establish the final write freeze or accepted-write journal.

The private production Cloudflare R2 bucket baseline and the empty PostgreSQL
foundation on Cyphers VPS were provisioned on 2026-07-27. Durable jobs will
initially use PostgreSQL rather than a separate queue service. The R2
bucket-scoped runtime credential will be created only when the production
service can consume it from the protected server environment. The invisible
same-installation session contract and canonical PostgreSQL schema were
specified and the migration was validated in a disposable database without
changing production. The version-one installation registration, silent session
proof, and revocation primitives now exist in `noise-core` with fixed
cross-platform vectors. The next implementation work is the central API. The
importer should be built only after the verifier can reproduce every aggregate
and digest in this document from copied data.
