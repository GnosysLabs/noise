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
cryptographically verifies reports only when rendering that private page. It
does not render or download reported media, and reviewed state is stored as
private sidecar files under the ignored development inbox.

During development, the intake and reviewer can share this local directory. A
production reviewer must remain unreachable from the internet and obtain only
encrypted envelopes through an outbound pull or another authenticated private
transfer. The public intake must never receive or mount the private recipient
key.

The client development build uses `http://127.0.0.1:4310`. A production build
must configure both `VITE_NOISE_SAFETY_URL` and the pinned
`VITE_NOISE_SAFETY_PUBLIC_KEY`; it will not trust a key fetched from a remote
intake.
