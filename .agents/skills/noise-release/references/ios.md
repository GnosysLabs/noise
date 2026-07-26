# Native iOS

## Repositories and identities

- iOS repository: `/Users/christopher/Dev/noise-ios`
- Shared Rust core: `/Users/christopher/Dev/noise`
- Project generator: `xcodegen`
- App bundle: `xyz.gnosyslabs.noise.ios`
- Notification service extension: `xyz.gnosyslabs.noise.ios.SignalNSE`
- App Group: `group.xyz.gnosyslabs.noise.group`
- Team: `4PDUNTF69S`
- Preferred toolchain: `/Applications/Xcode-beta.app/Contents/Developer`

The app and extension versions/build numbers in `project.yml` must match. The extension bundle identifier must remain prefixed by the app bundle identifier.

## Generate and build

```sh
cd /Users/christopher/Dev/noise-ios
xcodegen generate
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer \
  xcodebuild \
  -project Noise.xcodeproj \
  -scheme Noise \
  -configuration Debug \
  -destination 'generic/platform=iOS' \
  -allowProvisioningUpdates \
  build
```

The Xcode pre-build phase compiles `noise-ffi` from the sibling core repository. Record both repository revisions/dirty trees for an iOS artifact.

## Physical-device install

Discover the current device identifier; do not reuse a stale ID:

```sh
xcrun devicectl list devices
xcrun xctrace list devices
xcrun devicectl device install app --device <CoreDevice-ID> <path>/NoisePrivateGroups.app
xcrun devicectl device process launch --device <CoreDevice-ID> --terminate-existing xyz.gnosyslabs.noise.ios
```

Physical installation is mandatory validation for embedded-extension packaging. Simulator or generic build success does not catch all extension/provisioning failures.

## Notification extension invariants

Keep these in `project.yml`, then regenerate:

- `NSExtensionPointIdentifier = com.apple.usernotifications.service`
- `NSExtensionPrincipalClass = $(PRODUCT_MODULE_NAME).NotificationService`
- shared App Group entitlement

Do not leave only `info.path` in XcodeGen. Without the `info.properties.NSExtension` dictionary, regeneration can erase the extension metadata and physical installation fails with `AppexBundleMissingNSExtensionDict`.

The main target requires Push Notifications, Communication Notifications, and App Groups capabilities. The extension requires the same App Group. Verify the embedded extension and signed entitlements in the archive, not just the YAML.

## Release archive and App Store Connect

1. Increment and align `MARKETING_VERSION`/`CURRENT_PROJECT_VERSION` for both targets.
2. Regenerate the Xcode project.
3. Archive Release:

```sh
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer \
  xcodebuild \
  -project Noise.xcodeproj \
  -scheme Noise \
  -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath <explicit-output>/Noise.xcarchive \
  -allowProvisioningUpdates \
  archive
```

4. Inspect the archive, embedded extension, bundle IDs, versions, provisioning profiles, and signed entitlements.
5. Export/upload using an explicitly reviewed App Store Connect export configuration or Xcode Organizer. No `ExportOptions.plist` is currently canonical in the repository, so do not invent one silently.
6. Submit or distribute only with explicit authorization.

## Push secrets

The APNs key is external to the repositories. Its current identifier is `HFYLVC3UAG`; never print or commit the `.p8` contents. The relay APNs topic must equal `xyz.gnosyslabs.noise.ios`.

## Verify

On a physical iPhone, verify launch, login, group/topic loading, media, DMs, background/terminated notifications, notification sender name/avatar enrichment, and notification taps. A successful upload or simulator run is not acceptance evidence.
