# noise safety intake

This is the separate, write-only intake boundary for reports sent to noise
Safety. It accepts only HPKE-sealed `SealedSafetyReportV1` envelopes and stores
them without decrypting, previewing, emailing, or logging report contents.

It is currently a development service, not a deployed public endpoint or a
management dashboard.

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

The client development build uses `http://127.0.0.1:4310`. A production build
must configure both `VITE_NOISE_SAFETY_URL` and the pinned
`VITE_NOISE_SAFETY_PUBLIC_KEY`; it will not trust a key fetched from a remote
intake.
