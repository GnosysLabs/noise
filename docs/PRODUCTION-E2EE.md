# Noise production encryption

Status: implementation in progress. This document is a release gate, not a
claim that the current client already provides these properties.

## Product requirements

Noise groups are persistent rooms. The encryption design must preserve all of
the following:

1. Relays never receive plaintext message or media content.
2. A newly admitted member can read the complete group history.
3. A removed, banned, or departed member cannot decrypt anything created after
   the membership change.
4. Adding or removing a member must not require encrypting one copy of every
   message for every member.
5. Offline members can catch up from replaceable relays.
6. The same account can be restored with its Noise ID and password.
7. A relay cannot forge membership, messages, or epoch changes.

Released clients do not yet meet requirement 3. The implementation in this
working tree replaces the long-lived secret for new events, but it remains
behind the relay/client upgrade and release gates below.

## Control plane: RFC 9420 MLS

Noise uses Messaging Layer Security (MLS) for group membership and epoch key
agreement. The selected implementation is OpenMLS 0.8.1 or newer within the
compatible 0.8 security line, using:

- MLS 1.0 / RFC 9420;
- X25519 HPKE;
- ChaCha20-Poly1305;
- SHA-256; and
- Ed25519 credentials bound to a Noise identity.

Every accepted membership commit advances the MLS epoch. An add commit admits
the new identity. A remove commit excludes the old identity from the new epoch.
The epoch exporter derives a Noise archive root that only members of that epoch
can obtain.

Membership commits are serialized by the group control log. Clients must not
merge two competing commits for the same parent epoch. The first production
implementation permits only the founder to author an epoch commit. Members
publish signed self-removal requests and moderators publish signed ban
requests; a founder client converts valid requests into MLS removal commits.
This constraint can later be replaced by a quorum control log without changing
message encryption.

## History plane: backward-readable archive roots

MLS intentionally prevents a new member from decrypting messages from epochs
before they joined. Noise intentionally grants full history, so archived
content uses a second layer.

For MLS epoch `N`, every member derives:

```text
archive_root_N = MLS-Exporter(
  "xyz.gnosyslabs.noise.archive-root.v1",
  group_id,
  32
)
```

After advancing from epoch `N-1` to epoch `N`, the committer publishes a signed
history link:

```text
AEAD(
  key = archive_root_N,
  plaintext = archive_root_(N-1),
  aad = group_id || N || (N-1)
)
```

A member who has `archive_root_N` can open the link to `N-1`, then continue
backward through the history. Someone removed in `N-1` knows the old root but
cannot reverse the AEAD link to obtain `archive_root_N`.

This provides the Noise product semantics:

- a new member receives the newest root and can walk backward through the room;
- a removed member cannot walk forward into new epochs; and
- relays only store signed ciphertext and opaque links.

Full history means a currently authorized member—or an attacker controlling
that member's unlocked device—can read the history. That is an intentional
product tradeoff and must not be described as forward secrecy for archived
content. MLS still provides forward-secure epoch transitions for the control
plane.

## Message envelopes

Each message or group event is:

1. serialized as a versioned Noise event payload;
2. encrypted with XChaCha20-Poly1305 under the archive root for its epoch using
   a fresh 192-bit nonce;
3. bound through AEAD additional data to the group ID, MLS epoch, author public
   key, author sequence, and event type; and
4. signed by the author's Noise identity.

Clients accept an event only when:

- its signature and content-derived event ID are valid;
- its epoch belongs to the authenticated group control log;
- the author was an active member in that epoch;
- the sequence is fresh for that author; and
- the application-level authorization rules accept the event.

## Join by frequency

A 12-digit frequency is a rendezvous code, not a lasting group encryption key.
It locates and authenticates a short-lived join capability. It never becomes an
archive root or MLS secret.

The production join flow is:

1. the joining client creates an MLS KeyPackage;
2. the frequency opens an encrypted join capability;
3. the client publishes a signed join request containing that KeyPackage;
4. the group controller validates the capability and current ban state;
5. the controller publishes an MLS add commit and Welcome; and
6. the joining client enters the new epoch and receives its archive root.

Noise automatically revokes and replaces the join capability when a member is
banned. Otherwise a banned person who retained the old frequency could simply
create another identity and request admission again.

The 12-digit code must use an augmented PAKE or equivalent rate-limited
rendezvous design before it is treated as resistant to offline guessing. A
hash-derived locator plus ciphertext is not sufficient for the 10^12 code
space.

## Existing-group migration

Migration is a coordinated hard cut because the initial network is small:

1. upgraded clients publish MLS KeyPackages for their Noise identities;
2. the founder creates the MLS group with the existing Noise group ID;
3. the founder adds every upgraded active member;
4. the signed epoch-zero genesis wraps the legacy group secret under the first
   MLS-derived archive root;
5. new clients stop publishing legacy events; and
6. once the cutover is confirmed, relays reject newly authored legacy events.

The legacy secret remains able to open legacy history by design. It cannot
decrypt any event authored after cutover.

If an active member has not upgraded, the UI must name that member and block
the cutover or require the founder to explicitly remove them. Noise must never
silently create a secure subgroup while showing the old membership list.

## Direct messages

The current static Diffie-Hellman DM secret is also a migration blocker. DMs
will use a two-member MLS group with the same epoch and archive-link structure.
This gives Noise one multi-device-capable key-management engine for groups and
DMs. DM history remains available to newly restored authorized devices, while
thread deletion and account removal continue to use signed deletion events.

## Persistence and devices

MLS state contains secret key material. Each installation uses a distinct MLS
leaf whose credential is signed by the long-lived Noise account identity.
Password sign-in therefore remains sufficient to authorize a new installation
without making two devices share mutable ratchet state. Removing an account
removes all of its leaves; later device management can revoke one certified
leaf without changing the account's public Noise identity.

Private MLS state never enters the synchronized account vault. It must live
only in an encrypted local browser/device vault. The synchronized account vault
may carry public device credentials and group pointers. A newly signed-in
device publishes its certified KeyPackage; an existing founder device
automatically admits it to the current epoch, whose archive root unlocks earlier
history through the backward links. A recovery path that does not depend on an
older founder device is still required before password-only restoration can be
called complete.

The current desktop client stores local state in a permission-restricted but
unencrypted JSON file. That is a production blocker. Before MLS is enabled for
real conversations, macOS and Windows must wrap local state with
platform-backed secret storage and the web client must encrypt IndexedDB state
with a non-exportable Web Crypto key. Old MLS key material removed by OpenMLS
must also disappear from the next encrypted local snapshot.

## Release gates

Noise must not call this production encryption until all of these are true:

- current members can exchange events after an MLS cutover;
- a new member can decrypt pre-join history;
- a removed member cannot decrypt post-removal events;
- offline members can process ordered commits and catch up;
- competing same-epoch commits fail closed instead of silently forking;
- account restore retains current MLS state without restoring erased old state;
- MLS private state is encrypted at rest and is never copied into the
  synchronized account vault;
- web, macOS, and Windows use the same vectors and protocol version;
- corrupted, replayed, reordered, and forged control records are rejected;
- the upgrade path is exercised against a copy of real relay history; and
- the protocol and implementation receive an independent security review.
