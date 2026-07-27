# noise safety reporting foundation

This document defines the protocol boundary for escalating a report from an
official noise client to noise safety. The category-first client flow and the
separate write-only development intake now exist, but the intake is not yet
deployed as a public production service.

## Authority boundary

noise keeps three different responsibilities separate:

1. Group founders and moderators handle ordinary community rules.
2. Members can hide, block, leave, and report without moderator permission.
3. noise safety handles severe platform-wide concerns and reports where group
   staff are involved, unsafe to contact, or have not acted.

Independent relays remain availability infrastructure. They do not receive
plaintext, decide whether a report is valid, or gain authority over a group.

## Routing

Official clients expose one **Report** action followed by a category:

- group rules;
- harassment or hateful behavior;
- spam, scam, or impersonation;
- threats or immediate danger;
- sexual exploitation or non-consensual sexual content;
- child safety;
- sexually explicit content in a group that is not properly labeled; or
- something else.

Consensual sexually explicit material and groups labeled for it are allowed.
They are not a noise-level violation merely because they contain intimate or
explicit content. The sexual-safety category is deliberately limited to
exploitation and non-consensual content. Correctly labeled consensual explicit
content is not reportable merely for being explicit. Sexually explicit content
in a general group is reportable because it bypasses the member's content
choice.

## Adult access at launch

noise launches for adults aged 18 and older. New official-client accounts
enter a birth date before identity keys are generated or any relay request is
made. The client computes only whether the person is at least 18 and discards
the exact date. The encrypted account vault stores the successful attestation
and the explicit-content preference.

The small set of accounts created before this gate is deliberately
grandfathered as known adults. They are not asked to re-enter a date. Explicit
groups still begin hidden for every account, including grandfathered ones.

Adult access and explicit-content visibility are separate:

- an account must pass the 18+ gate;
- explicit groups are hidden by default;
- the member must explicitly enable them in Content settings;
- explicit groups carry a visible **EXPLICIT** label;
- a founder may permanently upgrade a general group to explicit; doing so revokes
  its previous unlabeled invitation and generates a replacement frequency, and
  no client accepts a later downgrade; and
- official clients refuse to join, open, search, or display explicit groups while
  the preference is off.

The preference is account-synced so each official client can honor it.
Platform-specific builds may restrict where the preference can be changed when
their storefront rules require that, while still honoring the synced value.
This is an official-client policy gate, not a claim that a self-declared date
or open-source client is cryptographic proof of age.

Ordinary group-rule, harassment, and spam reports route to group staff by
default. Severe categories route to noise safety. Every group-staff route must
offer **Report to noise instead** when group staff are involved or the member
feels unsafe reporting to them.

A report to noise is never published into the group's moderation history and
does not notify its founder or moderators.

## `SafetyReportV1`

`SafetyReportV1` is a signed, content-minimized report containing:

- a random report id, version, category, and creation time;
- the original `SignedEvent`, which remains encrypted but allows verification
  of its group id, event id, author public key, author signature, and timestamp;
- the reporter public key and signature;
- an optional reporter-authored signed event providing group context;
- an optional SHA-256 fingerprint computed locally over reported media;
- optional opaque encrypted-object and shard locations;
- the exact text of the single reported message, when present, up to the normal
  10,000-character message limit; and
- an optional short reporter statement.

The app tells the reporter that the exact reported text will be sent inside the
encrypted safety report and must not add surrounding chat history. Text is
necessary to assess harassment, scams, threats, and other text violations. The
schema has no media attachment field, media key, deletion capability, thumbnail,
or payload byte field. An ordinary hash does not establish that media is
illegal. It can identify exact bytes, link duplicates, or match a separately
governed trusted hash source.

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
dedicated, rotatable noise safety HPKE key using:

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
report contents. The public intake service does not hold the recipient secret;
that key belongs in a separate private review worker.

## Data handling

The development intake under `noiseSaftey/` accepts only the sealed envelope,
enforces a strict size limit and per-address rate limit, and writes the
ciphertext to a private spool without decrypting it. It runs with a public
recipient-key file; the recipient secret is generated into a separate file and
is not required by the intake.

The separate development reviewer binds only to loopback, requires an ephemeral
unguessable URL token, reads the private recipient key with restrictive file
permissions, and decrypts and cryptographically verifies envelopes from a local
inbox. It renders reported text and metadata but never media bytes, previews, or
keys. Reviewed state is stored separately from the immutable envelope. In
production, this reviewer must obtain still-encrypted envelopes through an
outbound pull or another authenticated private transfer; it must not expose an
internet listener.

The private case store must not create media previews, accept manual uploads,
place reports in email, or copy report contents into general application logs.

Opening a report verifies cryptographic facts, not the legal character of
unknown media. Temporary suppression and quarantine decisions must be recorded
as precautionary actions unless evidence has been verified through an approved
process.

## Deferred work

This foundation intentionally does not yet implement:

- deployment of the write-only intake endpoint;
- production transfer between the public intake and private reviewer;
- signed official-client suppression or group-quarantine decisions;
- trusted hash integrations; or
- legal evidence and external reporting procedures.
