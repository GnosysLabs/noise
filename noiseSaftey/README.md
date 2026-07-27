# noise safety intake

This is the separate, write-only intake boundary for reports sent to noise
safety. It accepts only HPKE-sealed `SealedSafetyReportV1` envelopes and stores
them without decrypting, previewing, emailing, or logging report contents.

It is currently a development service, not a deployed public endpoint.

Generate separate public and private recipient-key files:

```sh
cargo run -p noise-safety-intake -- keygen \
  --public-output noiseSaftey/dev-data/recipient-public.json \
  --secret-output noiseSaftey/dev-data/recipient-secret.json
```

Run the public intake with only the public file:

```sh
cargo run -p noise-safety-intake -- serve \
  --public-key-file noiseSaftey/dev-data/recipient-public.json \
  --spool-dir noiseSaftey/dev-data/inbox
```

Run the private reviewer on a separate loopback port:

```sh
cargo run -p noise-safety-intake -- review \
  --secret-key-file noiseSaftey/dev-data/recipient-secret.json \
  --spool-dir noiseSaftey/dev-data/inbox
```

The reviewer prints a random, single-session localhost URL. It decrypts and
cryptographically verifies reports only inside that private service. New
reports include encrypted human context: names displayed at report time, noise
signatures, the reporter's follow-up preference, the cryptographically
verifiable founder, and a reporter-signed moderator snapshot. Reports created
before that context was added still show noise signatures, but their names
remain unavailable.

The reviewer never renders or retrieves reported media. A reviewer may
deliberately download the complete decrypted and verified report as JSON; that
file can contain reported message text and private metadata, but never media
bytes or decryption keys.

Each report can be closed with no action or produce one of these signed,
content-free directives:

- suppress the reported event;
- restrict the group for 24 hours;
- restrict the group indefinitely;
- restrict the reported identity indefinitely.

The directive protocol also supports signed group and identity restores.
Temporary and indefinite group restrictions use the same client enforcement
mechanism; only the optional expiry differs.

Local decisions are stored under `.review-state/decisions`. Enforcement
directives are reconciled into `.review-state/directive-outbox` so a crash
between the decision and outbox write cannot lose an action. The directive
signer is deterministically derived from the existing recipient secret with a
separate KDF context, preserving compatibility with existing encrypted inbox
files.

The outbox is not a public directive feed yet, and official noise apps do not
consume or enforce these files yet.

During development, the intake and reviewer can share this local directory. A
production reviewer must remain unreachable from the internet and obtain only
encrypted envelopes through an outbound pull or another authenticated private
transfer. The public intake must never receive or mount the private recipient
key.

The client development build uses `http://127.0.0.1:4310`. A production build
must configure both `VITE_NOISE_SAFETY_URL` and the pinned
`VITE_NOISE_SAFETY_PUBLIC_KEY`; it will not trust a key fetched from a remote
intake.
