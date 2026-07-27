# Windows desktop

## Canonical release pipeline

The canonical Windows release machine is the user's PC over Tailscale SSH:

```sh
scripts/release-windows-remote.sh <exact-commit>
```

The Mac-side script:

1. Requires the exact commit to be present on `origin/main`.
2. Connects through the `noise-windows` SSH alias.
3. Uses the updater password stored for the Windows account with DPAPI. If it
   is missing, the Mac provisions it once from the existing macOS Keychain
   entry without printing it.
4. Runs `scripts/release-windows.ps1` in a temporary detached worktree on the
   PC, leaving the PC's main checkout untouched.
5. Builds the NSIS installer with Tauri updater signing enabled.
6. Copies the installer and `.sig` back to `target/release/windows-assets/`.
7. Verifies the revision, version, PE GUI subsystem, and SHA-256 hashes.

The PC checkout is `C:\Users\cmcel\noise`. Its ignored build output and old log
files are not release source. Do not reset or clean that checkout to prepare a
release.

## GitHub fallback

`.github/workflows/client-windows.yml` is an unsigned manual fallback. It
installs Node 22, pnpm 10.30.3, stable Rust, disables updater artifacts, and
builds an x64 NSIS installer. It is useful for diagnosing whether a clean
Windows runner compiles, but it is not the normal release path.

## Important signing status

The PC has the noise Tauri updater key, so the canonical workflow produces the
updater `.sig`. It currently has no Windows Authenticode certificate in the
current user's certificate store. The updater signature proves the artifact is
authorized by noise's updater key; it does not remove Windows SmartScreen's
unsigned-publisher warning.

Before claiming Authenticode publisher signing, explicitly configure and
verify:

1. An accessible code-signing certificate without exposing its private key or password.
2. A timestamp server and Tauri Windows signing configuration.

The release still requires a Windows platform entry in the same final
`latest.json` used by the desktop release.

## Build options

- For a release artifact, run `scripts/release-windows-remote.sh` with the same
  accepted revision used for macOS.
- For an unsigned fallback artifact, dispatch `Windows client (unsigned
  fallback)` and download its `noise-windows-<sha>` artifact.

## Verify

- Inspect the exact installer with Windows signature properties or `Get-AuthenticodeSignature`.
- Install it on Windows and launch the installed executable.
- Verify the displayed version, requested behavior, uninstall entry, and updater behavior.
- Download the published installer and updater artifact from their public URLs; confirm byte hashes match the locally accepted artifacts.

If any signing item is absent, report “Windows test installer built; public signing incomplete.”
