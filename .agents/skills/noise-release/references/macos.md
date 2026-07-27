# macOS desktop

## Canonical configuration

- Client: `/Users/christopher/Dev/noise/apps/client`
- Tauri config: `apps/client/src-tauri/tauri.conf.json`
- Release script: `scripts/release-macos.sh`
- Bundle identifier: `xyz.gnosyslabs.noise`
- Signing identity: `Developer ID Application: Christopher McElvogue (4PDUNTF69S)`
- Updater key default: `/Users/christopher/.tauri/noise.key`
- Updater password keychain service: `xyz.gnosyslabs.noise.updater`
- Notary profile default: `AC_NOTARY`

Never print the updater private key or password. Confirm that `noise.key.pub` matches the public key in `tauri.conf.json`.

## Development

Run the live desktop app:

```sh
pnpm --dir apps/client install --frozen-lockfile
pnpm --dir apps/client dev:desktop
```

Vite listens on `127.0.0.1:1420`; frontend hot reload remains active while
the Rust backend runs from `target/release/noise-desktop`. Keep the native
backend optimized: encrypted group rebuilding is fast in web's release WASM
but can take seconds in an unoptimized Rust binary. If hot reload appears
stale, verify the running process start time and executable mtime, then restart
the dev command.

## Signed release build

Ensure at least 10 GiB is free and the Developer ID identity, notary profile, updater key, and updater password are available. Then run:

```sh
NOISE_RELEASE_NOTES="Describe this release." scripts/release-macos.sh "vX.Y.Z"
```

The script:

1. Preflights the signing identity, notary profile, and updater signature.
2. Builds `noise.app`.
3. Submits it for notarization and staples the ticket.
4. Creates a human ZIP, updater tarball, updater signature, and `latest.json`.
5. Verifies codesigning, Gatekeeper assessment, archives, and manifest shape.

Artifacts are written to `target/release/release-assets/`.

## Publish

Inspect every artifact and confirm the tag matches the Tauri version. Only after explicit publication authorization, create or update the desktop GitHub release with:

- `noise-X.Y.Z-macOS-arm64.zip`
- `noise-X.Y.Z-macOS-arm64.app.tar.gz`
- `noise-X.Y.Z-macOS-arm64.app.tar.gz.sig`
- the final combined `latest.json`

The release URL embedded in `latest.json` must match the actual tag and filename. Do not mark a `relay-v*` release Latest.

## Verify

- Run `codesign --verify --deep --strict --verbose=2 noise.app`.
- Run `spctl --assess --type execute --verbose=4 noise.app`.
- Install/extract the published ZIP, launch the packaged app, and exercise the requested behavior.
- Fetch the published `latest.json` and every referenced asset; confirm status 200 and signature presence.
- If testing auto-update, start from the prior installed version and verify the actual update path.
