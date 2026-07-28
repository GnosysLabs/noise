# noise safety reporting foundation

This document defines the protocol and operational boundary for escalating a
report from an official noise client to noise safety.

## Authority boundary

noise keeps three different responsibilities separate:

1. Group founders and moderators handle ordinary community rules.
2. Members can hide, block, leave, and report without moderator permission.
3. noise safety handles the small set of severe platform-wide concerns defined
   below.

Independent relays remain availability infrastructure. They do not receive
plaintext, decide whether a report is valid, or gain authority over a group.

## Routing

Official clients expose one **Report** action followed by a category. These go
to the group founder and moderators:

- group rules;
- harassment or hateful behavior;
- spam, scam, or impersonation;
- sexual content or nudity in a group that is not properly labeled; and
- something else.

These go privately to noise safety:

- a specific, credible threat of violence made through noise;
- sexual exploitation or non-consensual sexual content; and
- child safety.

The threat screen tells the reporter to contact local emergency services when
anyone is in immediate danger. noise safety is not an emergency service.

Consensual sexual content or nudity and groups labeled for it are allowed.
They are not a noise-level violation merely because they contain intimate or
sexual content or nudity. The sexual-safety category is deliberately limited to
exploitation and non-consensual content. Correctly labeled consensual sexual
content or nudity is not reportable merely because it is sexual. Sexual content
or nudity in a general group is reportable because it bypasses the member's
content choice.

## Adult access at launch

noise launches for adults aged 18 and older. New official-client accounts
enter a birth date before identity keys are generated or any relay request is
made. The client computes only whether the person is at least 18 and discards
the exact date. The encrypted account vault stores the successful attestation
and the sexual-content visibility preference.

The small set of accounts created before this gate is deliberately
grandfathered as known adults. They are not asked to re-enter a date. Groups
labeled for sexual content or nudity still begin hidden for every account,
including grandfathered ones.

Adult access and sexual-content visibility are separate:

- an account must pass the 18+ gate;
- groups labeled for sexual content or nudity are hidden by default;
- the member must enable them in Content settings;
- groups labeled for sexual content or nudity carry a visible flame marker;
- a founder may permanently mark a general group for sexual content or nudity;
  doing so revokes its previous unlabeled invitation and generates a replacement
  frequency, and no client accepts a later downgrade; and
- official clients refuse to join, open, search, or display those groups while
  the preference is off.

The preference is account-synced so each official client can honor it.
Platform-specific builds may restrict where the preference can be changed when
their storefront rules require that, while still honoring the synced value.
This is an official-client policy gate, not a claim that a self-declared date
or open-source client is cryptographic proof of age.

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

Text is necessary to assess a specific threat and other text violations. The
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

## End-to-end workflow

1. A member chooses **Report**, selects a category, and optionally adds a short
   statement. The reported message disappears from that member's view
   immediately. The app also offers block and leave.
2. An ordinary category becomes a group moderation event for the founder and
   moderators. A severe category becomes an HPKE-sealed report sent to the
   write-only public intake. Media bytes, previews, and decryption keys are
   never included.
3. The public intake stores only the encrypted envelope. It cannot decrypt or
   preview the report.
4. A reviewer pulls encrypted envelopes to the private reviewer, which binds
   only to localhost. Opening a case verifies the report, reporter, original
   event, and available group context. The reviewer sees the single reported
   message's text and the submitted statement, but never retrieves media.
5. The reviewer closes the case with no action, hides the reported message,
   pauses the group for 24 hours, blocks the group indefinitely, or blocks the
   reported author indefinitely.
6. An enforcement action creates a signed, content-free directive. Only its
   target identifiers, policy reason, issue time, and optional expiry are
   uploaded to the public `/v1/directives` feed.
7. Official clients poll that feed, verify every directive against their pinned
   signing key, and merge it into durable last-known local state. A missing,
   empty, or truncated response never removes an already learned directive.
8. A later signed group or identity restore supersedes an indefinite
   restriction. A temporary group restriction stops applying at its signed
   expiry.

## Official-client enforcement and cache purge

The client persists a newly verified directive before deleting cached content.
It records the purge as complete only after deletion succeeds, so a crash or
filesystem failure leaves the purge pending for the next safety sync.

- **Hide message:** remove the event from official conversation views, unread
  state, and reports; delete that group's complete-file and decrypted-chunk
  media caches.
- **Pause or block group:** replace the group with the neutral unavailable
  state and delete that group's decrypted media caches.
- **Block author:** remove that author's content, clear the shared profile-image
  cache, delete the author's direct-message media scope, and delete all known
  group media caches.

The browser keeps decrypted chunks only in a memory LRU, so enforcement clears
that memory cache. Native and browser downloads carry a cache generation; a
download that began before a purge cannot refill the old cache afterward.
Attachment fetches check current safety state before and after retrieval, so a
stale screen cannot rebuild media hidden by a directive.

The group-level deletion used for message and identity actions is intentionally
conservative. A hidden message may evict unrelated allowed media from its
group, and a blocked author evicts group media across the account because old
media can outlive the compacted event window that identified its author.
Allowed media can be downloaded again. This is preferable to retaining a
prohibited attachment because a cache entry could not be mapped perfectly.

## Limits and deferred work

An official-client directive cannot erase encrypted objects from independent
storage nodes, delete copies another person already saved, or force an
unofficial client to comply. It makes the content unavailable through official
noise apps and removes decrypted local cache material those apps control.

Trusted known-content hash integrations, a formal legal evidence process, and
external reporting procedures remain separate future work. They are not
implied by opening a report or issuing a precautionary directive.
