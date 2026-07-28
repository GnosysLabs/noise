# noise production encryption

Status: implementation in progress. This document is a release gate, not a
claim that the current client already provides these properties.

## Product requirements

noise groups are persistent rooms. The encryption design must preserve all of
the following:

1. Relays never receive plaintext message or media content.
2. A newly admitted member can read the complete group history.
3. A removed, banned, or departed member cannot decrypt anything created after
   the membership change.
4. Adding or removing a member must not require encrypting one copy of every
   message for every member.
5. Offline members can catch up from replaceable relays.
6. The same account can be restored with its noise ID and password.
7. A relay cannot forge membership, messages, or epoch changes.

Released clients do not yet meet requirement 3. The implementation in this
working tree replaces the long-lived secret for new events, but it remains
behind the relay/client upgrade and release gates below.

## Control plane: RFC 9420 MLS

noise uses Messaging Layer Security (MLS) for group membership and epoch key
agreement. The selected implementation is OpenMLS 0.8.1 or newer within the
compatible 0.8 security line, using:

- MLS 1.0 / RFC 9420;
- X25519 HPKE;
- ChaCha20-Poly1305;
- SHA-256; and
- Ed25519 credentials bound to a noise identity.

Every accepted membership commit advances the MLS epoch. An add commit admits
the new identity. A remove commit excludes the old identity from the new epoch.
The epoch exporter derives a noise archive root that only members of that epoch
can obtain.

Membership commits are serialized by the group control log. Clients must not
merge two competing commits for the same parent epoch.

A frequency holder authors its own MLS external commit. The central service
accepts that commit only when the authenticated account presents the current
invitation locator, the commit extends the exact current control head, and it
only adds the joining account. No existing group member or device participates
in admission.

Only the founder may author a commit that removes accounts: members publish
signed self-removal requests, moderators publish signed ban requests, and a
founder client converts valid requests into MLS removal commits.

The central transaction lock admits only one child for a control head.
Concurrent external joins automatically refetch the winning head and create a
new external commit; the user is not asked to retry.

## History plane: backward-readable archive roots

MLS intentionally prevents a new member from decrypting messages from epochs
before they joined. noise intentionally grants full history, so archived
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

This provides the noise product semantics:

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

1. serialized as a versioned noise event payload;
2. encrypted with XChaCha20-Poly1305 under the archive root for its epoch using
   a fresh 192-bit nonce;
3. bound through AEAD additional data to the group ID, MLS epoch, author public
   key, author sequence, and event type; and
4. signed by the author's noise identity.

Clients accept an event only when:

- its signature and content-derived event ID are valid;
- its epoch belongs to the authenticated group control log;
- the author was an active member in that epoch;
- the sequence is fresh for that author; and
- the application-level authorization rules accept the event.

## Join by frequency

A 12-digit frequency is a rendezvous code, not an MLS epoch secret. It opens the
signed invitation and its history-wrapping key locally. That key is used only
to open the current external-join continuity package; message events remain
encrypted by MLS-derived archive roots.

The production join flow is:

1. the frequency opens the signed encrypted invitation locally;
2. the client fetches the current signed external-join package by invitation
   locator;
3. the frequency-held wrapping key opens the current archive root locally;
4. the joining client creates and signs its own MLS external commit;
5. the central service verifies the current invitation and atomically appends
   the commit; and
6. the epoch and its next continuity package commit in the same transaction,
   and the client is immediately active.

noise automatically revokes and replaces the join capability when a member is
banned. Otherwise a banned person who retained the old frequency could simply
create another identity and request admission again.

The 12-digit code must use an augmented PAKE or equivalent rate-limited
rendezvous design before it is treated as resistant to offline guessing. A
hash-derived locator plus ciphertext is not sufficient for the 10^12 code
space.

## Existing-group migration

Existing MLS groups require one upgraded current member to publish a
current-head continuity package once. The package contains public MLS
GroupInfo and the current archive root encrypted under the invitation's
history-wrapping key. It is signed by that member and stored by the central
service only after current-membership and head checks.

New groups publish this package with epoch zero. Every accepted epoch causes a
current client to replace it for the new head. Once the package exists, future
frequency joins are independent of member presence.

The legacy secret remains able to open legacy history by design. It cannot
decrypt any event authored after cutover.

If an active member has not upgraded, the UI must name that member and block
the cutover or require the founder to explicitly remove them. noise must never
silently create a secure subgroup while showing the old membership list.

## Direct messages

The current static Diffie-Hellman DM secret is also a migration blocker. DMs
will use a two-member MLS group with the same epoch and archive-link structure.
This gives noise one multi-device-capable key-management engine for groups and
DMs. DM history remains available to newly restored authorized devices, while
thread deletion and account removal continue to use signed deletion events.

## Persistence and devices

MLS state contains secret key material. noise keeps one recoverable MLS state
per account and group rather than requiring every installation to be admitted
as a new leaf. Each current per-group state is included in the synchronized
account vault, which is encrypted by the high-entropy key derived from the
noise ID and password and signed by the long-lived account identity.

A newly signed-in installation restores those per-group states, verifies the
signed MLS control log, advances through any later commits, and immediately
unlocks the current archive root and its backward history links. No older
device, founder session, or manual approval is part of account restoration.
Account credentials are therefore the recovery and authorization boundary:
anyone who can successfully sign in can recover the account's current groups.
Leaving, banning, or deleting a group removes its recovery state from the next
vault revision.

The current desktop client stores local state in a permission-restricted but
unencrypted JSON file. That is a production blocker. Before MLS is enabled for
real conversations, macOS and Windows must wrap local state with
platform-backed secret storage and the web client must encrypt IndexedDB state
with a non-exportable Web Crypto key. Old MLS key material removed by OpenMLS
must also disappear from the next encrypted local snapshot.

## Release gates

noise must not call this production encryption until all of these are true:

- current members can exchange events after an MLS cutover;
- a new member can decrypt pre-join history;
- a removed member cannot decrypt post-removal events;
- offline members can process ordered commits and catch up;
- competing same-epoch commits fail closed instead of silently forking;
- account restore retains current MLS state without restoring erased old state;
- synchronized MLS recovery state is isolated per group and exists only inside
  the password-encrypted account vault or encrypted local storage;
- web, macOS, and Windows use the same vectors and protocol version;
- corrupted, replayed, reordered, and forged control records are rejected;
- the upgrade path is exercised against a copy of real relay history; and
- the protocol and implementation receive an independent security review.
