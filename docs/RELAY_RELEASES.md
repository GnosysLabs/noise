# Relay releases

Relay versions are independent from desktop app versions. A relay tag is named
`relay-vX.Y.Z`; publishing one must never replace the macOS/Windows release
marked **Latest** on GitHub.

## Trust model

GitHub hosts the packages and channel files, but it is not the root of trust.
Every relay binary embeds the Ed25519 public key in
`crates/noise-relay/src/update.rs`. The matching public PEM is committed at
`deploy/relay-release-public.pem`.

The private key lives outside the repository at:

```text
~/.config/noise/relay-release.pem
```

It must remain mode `600` and needs an offline backup. Losing it means existing
relay binaries cannot trust a newly rotated key without a manually installed
transition release.

## Cut a relay release

1. Update `version` in `crates/noise-relay/Cargo.toml` and update `Cargo.lock`.
2. Commit and push the finished relay changes.
3. Create and push the matching tag:

   ```sh
   git tag relay-vX.Y.Z
   git push origin relay-vX.Y.Z
   ```

4. The `Relay packages` workflow builds statically linked Ubuntu packages on
   native x86-64 and ARM64 runners and assembles one draft GitHub release.
5. After both `.deb` files exist in the draft, sign and publish it:

   ```sh
   scripts/promote-relay-release.sh relay-vX.Y.Z stable
   ```

6. Inspect, commit, and push the generated exact-byte channel files:

   ```text
   deploy/relay-channels/stable.json
   deploy/relay-channels/stable.json.sig
   ```

The promotion script publishes the relay release with `latest=false`, so the
desktop download and Tauri updater continue to resolve to the current desktop
release.

## Protocol transitions

Do not require a new protocol on the same release that first introduces it.
Ship an overlap release that understands both sides of the transition, allow
the signed updater timer to propagate it, and only then publish a release that
retires the old protocol. Independently operated relays cannot and should not
be force-upgraded.

The relay `/health` response and `status --json` expose both software and
protocol versions for agents and monitoring.
