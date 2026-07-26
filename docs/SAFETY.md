# Noise safety reporting foundation

This document defines the first protocol boundary for escalating a report from
an official Noise client to Noise Safety. It does not add a public endpoint,
management console, quarantine feed, or user-visible control yet.

## Authority boundary

Noise keeps three different responsibilities separate:

1. Group founders and moderators handle ordinary community rules.
2. Members can hide, block, leave, and report without moderator permission.
3. Noise Safety handles severe platform-wide concerns and reports where group
   staff are involved, unsafe to contact, or have not acted.

Independent relays remain availability infrastructure. They do not receive
plaintext, decide whether a report is valid, or gain authority over a group.

## Routing

Official clients will eventually expose one **Report** action followed by a
category:

- group rules;
- harassment or hateful behavior;
- spam, scam, or impersonation;
- threats or immediate danger;
- sexual exploitation or intimate imagery;
- child safety; or
- something else.

Ordinary group-rule, harassment, and spam reports route to group staff by
default. Severe categories route to Noise Safety. Every group-staff route must
offer **Report to Noise instead** when group staff are involved or the member
feels unsafe reporting to them.

A report to Noise is never published into the group's moderation history and
does not notify its founder or moderators.

## `SafetyReportV1`

`SafetyReportV1` is a signed, metadata-only report containing:

- a random report id, version, category, and creation time;
- the original `SignedEvent`, which remains encrypted but allows verification
  of its group id, event id, author public key, author signature, and timestamp;
- the reporter public key and signature;
- an optional reporter-authored signed event providing group context;
- an optional SHA-256 fingerprint computed locally over reported media;
- optional opaque encrypted-object and shard locations; and
- an optional short reporter statement.

The report schema has no plaintext message field, media attachment field,
media key, deletion capability, thumbnail, or payload byte field. An ordinary
hash does not establish that media is illegal. It can identify exact bytes,
link duplicates, or match a separately governed trusted hash source.

The reporter signature covers every field using canonical struct-order JSON
prefixed by the fixed context
`xyz.gnosyslabs.noise.safety-report.v1`. The safety service verifies the
original event signature, optional group-context signature, bounds, identifiers,
and reporter signature after opening the envelope.

The optional group-context event proves that the reporter signed an event for
the same group. It does not independently prove current membership or establish
that the reported content is illegal.

## Sealed transport

Before leaving the device, the complete signed report is encrypted to a
dedicated, rotatable Noise Safety HPKE key using:

- DHKEM(X25519);
- HKDF-SHA-256; and
- ChaCha20-Poly1305.

`SealedSafetyReportV1` exposes only:

- the envelope version;
- a derived recipient key id used for safe key rotation;
- the HPKE encapsulated key; and
- ciphertext.

TLS remains required for transport. HPKE additionally prevents relays,
reverse proxies, request logging, and ordinary web infrastructure from reading
report metadata. Only the private safety service holds the recipient secret.

## Data handling

The future intake service must accept only the sealed envelope and enforce a
strict size limit before decryption. The private case store must not create
media previews, accept manual uploads, place reports in email, or copy report
contents into general application logs.

Opening a report verifies cryptographic facts, not the legal character of
unknown media. Temporary suppression and quarantine decisions must be recorded
as precautionary actions unless evidence has been verified through an approved
process.

## Deferred work

This foundation intentionally does not yet implement:

- the public write-only intake endpoint;
- case storage or the private management console;
- user-interface routing and confirmation;
- signed official-client suppression or group-quarantine decisions;
- trusted hash integrations; or
- legal evidence and external reporting procedures.
