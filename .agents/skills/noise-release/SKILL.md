---
name: noise-release
description: Build, sign, package, deploy, release, or verify noise across the React/Tauri macOS and Windows desktop clients, the web client, the native SwiftUI iOS app, and noise relays. Use for development builds, production releases, notarization, updater artifacts, TestFlight/App Store archives, Windows installers, web deployment, relay deployment, rollback, or release troubleshooting in /Users/christopher/Dev/noise and /Users/christopher/Dev/noise-ios.
---

# noise release

Treat source state, build output, signature, published artifact, deployed state, and live behavior as separate evidence. Never report a release as complete until every requested layer has been verified.

## Start every operation

1. Resolve the requested platforms and whether the user wants build-only, local install, deployment, or public release.
2. Inspect `git status --short`, the current revision, versions, and existing release configuration. Preserve unrelated dirty changes.
3. Obtain explicit authorization before pushing shared branches/tags, publishing releases, replacing production files, submitting to App Store Connect, or changing live relay configuration.
4. Record the exact source revision or dirty-tree fingerprint used for every artifact. Never silently build different platforms from different source states.
5. Read only the relevant platform reference:
   - macOS desktop: [references/macos.md](references/macos.md)
   - Windows desktop: [references/windows.md](references/windows.md)
   - Web: [references/web.md](references/web.md)
   - Native iOS: [references/ios.md](references/ios.md)
   - Relays: [references/relays.md](references/relays.md)
   - Server access and production topology: [references/servers.md](references/servers.md)

For an all-platform release, preflight all targets first, then build macOS and Windows concurrently from the same accepted source state. Web, relay, and App Store deployments remain distinct publication actions.

For Windows, prefer the user's Tailscale-connected PC as the release builder.
Run `scripts/release-windows-remote.sh <exact-commit>` from the Mac; it uses the
`noise-windows` SSH alias, builds in a temporary worktree on the PC, and returns
the NSIS installer plus updater signature to
`target/release/windows-assets/`. The GitHub Actions Windows workflow is an
unsigned manual fallback, not the default release path. See
[references/windows.md](references/windows.md) for the host, signing, and
verification rules.

## Release invariants

- Never expose private-key, certificate, provisioning-profile, or password contents in commands, logs, patches, or chat.
- Use the existing credential locations and keychain entries; do not generate or rotate keys during an ordinary release.
- Do not commit generated secrets, `.p8` files, private PEM files, signing passwords, or export credentials.
- Back up an exact production target before any `rsync --delete`, binary replacement, or config replacement. Use explicit paths, never a broad directory or unresolved target.
- Prefer the repository scripts and workflows. Inspect them immediately before use because versions and output paths can drift.
- Keep desktop app releases and `relay-v*` releases separate. A relay release must never replace the desktop release marked Latest.
- For protocol transitions, ship an overlap relay release before requiring the new protocol.
- A successful compilation is not a runtime test. Verify the actual packaged app, installed device app, served web assets, or live relay requested by the user.
- Use tests sparingly. Run targeted preflights and artifact/runtime verification rather than broad test suites unless a failure points there.

## Version and source discipline

Before building, inspect:

```sh
git status --short
git rev-parse HEAD
node -p "require('./apps/client/src-tauri/tauri.conf.json').version"
node -p "require('./apps/client/package.json').version"
awk -F '"' '/^version = / { print $2; exit }' crates/noise-relay/Cargo.toml
```

For iOS, inspect both `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION` for the app and notification extension in `/Users/christopher/Dev/noise-ios/project.yml`. Keep each pair aligned, regenerate the Xcode project, and verify the generated settings.

Never alter versions merely to make a build pass. Version changes are release changes and belong in the accepted release scope.

## Verification report

Report each requested target independently:

| Target | Source | Built | Signed | Published/deployed | Live verification |
|---|---|---:|---:|---:|---:|
| macOS | revision/tree | result | codesign + notarization | GitHub/update manifest | installed launch/update |
| Windows | revision/tree | result | updater + Authenticode status | GitHub/update manifest | installed launch/update |
| Web | revision/tree | result | n/a | served asset hash | browser behavior |
| iOS | core + iOS revisions | result | distribution profile | App Store Connect/TestFlight | physical iPhone |
| Relay | revision/tree | result | signed channel/package | installed or channel | local/public health |

Call out any unverified cell plainly. Do not substitute source inspection for live evidence.

## Rollback

Prepare rollback before publication:

- macOS/Windows: retain the prior GitHub release assets and updater manifest.
- Web: retain the previous `/var/www/app.makenoise.chat` directory under a timestamped explicit backup path.
- iOS: App Store builds cannot be rolled back; retain the prior accepted build and use a higher build number for a fix.
- Relay: retain the prior binary/config for a direct hotfix, or retain the prior signed channel manifest for managed releases.

After rollback, repeat the same live verification used for deployment.
