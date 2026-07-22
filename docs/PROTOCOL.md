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
10. **Media is stored once, not fanned out per member.** Clients encrypt media
    locally and group events contain only authenticated references to
    content-addressed ciphertext. Profile photos are the first consumer of
    this transport.
11. **Groups have signed identities.** A frequency has a name, short
    description, and encrypted icon reference. Updates are events in the same
    replicated history rather than mutable relay-owned records.

## Profiles and encrypted blobs

A profile is signed by the same locally generated identity that authors group
events. Username, short bio, and an avatar reference are carried inside the
encrypted membership log. Profile changes are new events; relays cannot alter
them or attach a profile to a different public key.

Avatar bytes are encrypted locally with a random key and stored by ciphertext
hash. The key and content reference exist only inside the encrypted profile
event. Clients fetch and decrypt avatars lazily, so a 50,000-member group does
not insert 50,000 images into its event log.

The same blob primitive is intended to carry group media. Large files will use
an encrypted manifest referencing fixed-size encrypted chunks, allowing relay
replication, resumable transfer, progressive playback, and optional peer
seeding without making any relay authoritative or giving it plaintext.

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
- Blob storage is currently in memory and whole-object. Production media needs
  durable storage, fixed-size chunks, quotas, garbage collection, and encrypted
  thumbnail manifests.
- Direct relay connections expose network metadata. Onion routing is not yet
  implemented.
