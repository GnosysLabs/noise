# Windows desktop

## Current pipeline

The canonical workflow is `.github/workflows/client-windows.yml`. It installs Node 22, pnpm 10.30.3, stable Rust, and builds an x64 NSIS installer:

```powershell
pnpm --dir apps/client install --frozen-lockfile
'{"bundle":{"createUpdaterArtifacts":false}}' |
  Set-Content apps/client/src-tauri/tauri.ci.conf.json
pnpm --dir apps/client tauri build --config src-tauri/tauri.ci.conf.json --ci
```

The installer is normally under `target/release/bundle/nsis/*-setup.exe`.

## Important signing status

The checked-in Windows CI deliberately disables updater artifacts and does not configure an Authenticode certificate in `tauri.conf.json`. Its installer is suitable for testing, but it is not a complete signed public release.

Do not claim Windows is signed or updater-ready merely because the workflow passed. Before a public Windows release, explicitly configure and verify:

1. An accessible code-signing certificate without exposing its private key or password.
2. A timestamp server and Tauri Windows signing configuration.
3. A Tauri updater artifact signed by the existing noise updater key.
4. A Windows platform entry in the same final `latest.json` used by the desktop release.

Do not invent certificate thumbprints, passwords, remote Windows hosts, or signing commands. Resolve them from the authorized machine/secret store at release time and add them to repository automation rather than a one-off shell history.

## Build options

- For an ordinary test artifact, dispatch the `Windows client` GitHub workflow and download its `noise-windows-<sha>` artifact.
- For a user-authorized build on their Windows machine, first confirm the exact host, checkout revision, working-tree state, certificate availability, and output path. Build the same accepted revision as macOS.

## Verify

- Inspect the exact installer with Windows signature properties or `Get-AuthenticodeSignature`.
- Install it on Windows and launch the installed executable.
- Verify the displayed version, requested behavior, uninstall entry, and updater behavior.
- Download the published installer and updater artifact from their public URLs; confirm byte hashes match the locally accepted artifacts.

If any signing item is absent, report “Windows test installer built; public signing incomplete.”
