# Noise protocol invariants

These are product constraints, not implementation details.

1. **No PII is required.** An identity is a locally generated key plus a
   self-authored profile. The protocol never requires a phone number, email,
   legal name, address book, birthday, or location.
2. **Groups are entered by frequency.** There is no group directory, search,
   recommendation system, or public discovery endpoint.
3. **Relays provide availability, not authority.** A relay may store and
   forward opaque records, but it cannot author a member's message or mutate a
   signed group event.
4. **Relays are replaceable.** Clients may publish to and read from several
   relays. No identity or group belongs to a relay.
5. **Every event is signed.** Clients reject corrupt or forged events before
   displaying them.
6. **Payloads are opaque to transport.** The relay protocol does not depend on
   plaintext message bodies. The protocol lab uses a shared development group
   key; production group encryption remains a separate, audited design.
7. **Offline clients catch up.** History is an append-only set of signed events
   addressed by content hash. Clients merge and deduplicate records obtained
   from any relay.
8. **Membership belongs to the group.** Joining and leaving are encrypted,
   signed events in the replicated group history, not rows owned by a relay.
   Message authors are resolved through this history rather than trusted
   self-reported names.
9. **The scale target is 50,000 members.** No design may require an all-to-all
   connection mesh or one stored copy of each message per recipient.
10. **Media is erasure-coded, not globally mirrored or fanned out per member.**
    Clients encrypt media locally, split each encrypted chunk across a small
    relay constellation, and put only its encrypted manifest in the group
    event.
11. **Groups have signed identities.** A frequency has a name, short
    description, and encrypted icon reference. Updates are events in the same
    replicated history rather than mutable relay-owned records.

## Profiles and constellation storage

A profile is signed by the same locally generated identity that authors group
events. Username, short bio, and an avatar reference are carried inside the
encrypted membership log. Profile changes are new events; relays cannot alter
them or attach a profile to a different public key.

Avatar, background, image, audio, and video bytes use the same transport.
Clients encrypt media locally with a random key and split every encrypted
1 MiB chunk into Reed–Solomon shards. Relays are ranked per object with a keyed
rendezvous score, so placement is stable but cannot be predicted without the
encrypted reference. A 12-relay constellation uses 8 data shards and 4 parity
shards; any eight reconstruct the encrypted object. Smaller networks use the
same two-thirds threshold, with 1-of-2 as the unavoidable two-relay case.

The encrypted manifest records the threshold, opaque shard IDs, and providers.
Downloads race providers in parallel, discard corrupt shards by hash, and stop
after the reconstruction threshold. Relays store only their assigned raw shard
in local disk or S3 plus a small Turso metadata row. Shards are not included in
relay snapshots and are never gossiped relay-to-relay.

Each shard has a deletion capability derived from the media key. The relay
stores only its hash; an authorized client reveals the capability to erase the
shard. This keeps group IDs out of storage metadata while allowing message,
thread, account, and founder group deletion to remove referenced media.

## Group identity

New groups record their founder's public key in the encrypted invitation.
Legacy development groups derive the founder from their first valid membership
event. Only an active founder may currently publish group identity changes;
every client independently enforces that rule while rebuilding history. This is
an intentionally small first governance policy, not a permanent assumption
that all groups must have one owner.

## Current laboratory limitations

- A frequency contains only twelve decimal digits. The relay stores an
  encrypted invitation under a hash-derived locator, but an operator can still
  perform an offline search of the small code space. Production frequencies
  require a PAKE-style rendezvous design, substantially more entropy, or both.
- Group messages currently use one shared symmetric development key. There is
  no forward secrecy, post-compromise security, member removal, or epoch
  rotation. This is deliberately not presented as production E2EE.
- Relays validate signatures but cannot validate encrypted group membership.
  Clients rebuild membership and reject messages from inactive keys.
- Leaving is currently advisory because the development shared group key is not
  rotated. Removal and cryptographic revocation require the production epoch
  design.
- Relay snapshots are intentionally simple and unbounded. They exist to prove
  convergence and recovery before pagination and anti-entropy ranges are added.
- Relays durably store assigned encrypted media shards on local disk or an
  operator-selected S3-compatible backend. Operators can bound the allocation;
  autonomous repair and garbage collection for shards abandoned by interrupted
  uploads remain future work.
- Direct relay connections expose network metadata. Onion routing is not yet
  implemented.
