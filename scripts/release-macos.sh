#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
client_dir="$repo_root/apps/client"
config="$client_dir/src-tauri/tauri.conf.json"
version=$(node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).version)' "$config")
release_tag=${1:-"v$version"}
updater_key=${NOISE_UPDATER_KEY_PATH:-"/Users/christopher/.tauri/noise.key"}
keychain_account=$(id -un)
keychain_service=xyz.gnosyslabs.noise.updater
notary_profile=${APPLE_KEYCHAIN_PROFILE:-AC_NOTARY}
assets_dir="$repo_root/target/release/release-assets"
bundle_dir="$repo_root/target/release/bundle/macos"
app_bundle="$bundle_dir/Noise.app"
human_zip="$assets_dir/Noise-$version-macOS-arm64.zip"
updater_archive="$assets_dir/Noise-$version-macOS-arm64.app.tar.gz"
latest_json="$assets_dir/latest.json"
temporary_dir=$(mktemp -d /tmp/noise-release.XXXXXX)

cleanup() {
  rm -rf "$temporary_dir"
}
trap cleanup EXIT

if [[ ! -f "$updater_key" ]]; then
  echo "Missing updater signing key: $updater_key" >&2
  exit 1
fi

for required_command in cargo codesign ditto node pnpm security spctl tar xcrun; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    echo "Missing required command: $required_command" >&2
    exit 1
  fi
done

available_kb=$(df -Pk "$repo_root" | awk 'NR == 2 { print $4 }')
if (( available_kb < 10485760 )); then
  echo "At least 10 GiB of free disk space is required for a clean release build" >&2
  exit 1
fi

updater_password=${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}
if [[ -z "$updater_password" ]]; then
  updater_password=$(security find-generic-password -a "$keychain_account" -s "$keychain_service" -w)
fi

export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$updater_password"
export APPLE_SIGNING_IDENTITY=${APPLE_SIGNING_IDENTITY:-"Developer ID Application: Christopher McElvogue (4PDUNTF69S)"}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
# Apple's current linker can corrupt stripped proc-macro dylibs before the
# application link begins. The final .app is still code-signed and notarized.
export CARGO_PROFILE_RELEASE_STRIP=${CARGO_PROFILE_RELEASE_STRIP:-false}

configured_public_key=$(node -e 'const fs=require("fs"); console.log(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).plugins.updater.pubkey)' "$config")
key_file_public_key=$(tr -d '\r\n' < "$updater_key.pub")
if [[ "$configured_public_key" != "$key_file_public_key" ]]; then
  echo "The updater public key in tauri.conf.json does not match $updater_key.pub" >&2
  exit 1
fi

security find-identity -v -p codesigning | grep -F "$APPLE_SIGNING_IDENTITY" >/dev/null
xcrun notarytool history --keychain-profile "$notary_profile" --output-format json >/dev/null

preflight_file="$temporary_dir/updater-signing-preflight"
printf 'Noise updater signing preflight\n' > "$preflight_file"
export TAURI_SIGNING_PRIVATE_KEY_PATH="$updater_key"
pnpm --dir "$client_dir" tauri signer sign "$preflight_file" >/dev/null
unset TAURI_SIGNING_PRIVATE_KEY_PATH
if [[ ! -s "$preflight_file.sig" ]]; then
  echo "Updater signing preflight did not produce a signature" >&2
  exit 1
fi

rm -rf "$assets_dir"
mkdir -p "$assets_dir"

export TAURI_SIGNING_PRIVATE_KEY="$updater_key"
pnpm --dir "$client_dir" tauri build --bundles app
unset TAURI_SIGNING_PRIVATE_KEY

if [[ ! -d "$app_bundle" ]]; then
  echo "Tauri did not produce $app_bundle" >&2
  exit 1
fi

notary_zip="$temporary_dir/Noise-$version-notarization.zip"
ditto -c -k --sequesterRsrc --keepParent "$app_bundle" "$notary_zip"
xcrun notarytool submit "$notary_zip" --keychain-profile "$notary_profile" --wait
xcrun stapler staple "$app_bundle"
xcrun stapler validate "$app_bundle"

ditto -c -k --sequesterRsrc --keepParent "$app_bundle" "$human_zip"
COPYFILE_DISABLE=1 tar -C "$bundle_dir" -czf "$updater_archive" "Noise.app"
export TAURI_SIGNING_PRIVATE_KEY_PATH="$updater_key"
pnpm --dir "$client_dir" tauri signer sign "$updater_archive"
unset TAURI_SIGNING_PRIVATE_KEY_PATH
if [[ ! -s "$updater_archive.sig" ]]; then
  echo "Updater archive signature is missing" >&2
  exit 1
fi

release_url="https://github.com/GnosysLabs/noise/releases/download/$release_tag/$(basename "$updater_archive")"
release_notes=${NOISE_RELEASE_NOTES:-"First public alpha of Noise."}
pub_date=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
node - "$latest_json" "$version" "$release_url" "$updater_archive.sig" "$release_notes" "$pub_date" <<'NODE'
const fs = require("fs");
const [output, version, url, signaturePath, notes, pubDate] = process.argv.slice(2);
const signature = fs.readFileSync(signaturePath, "utf8").trim();
const manifest = {
  version,
  notes,
  pub_date: pubDate,
  platforms: {
    "darwin-aarch64": { url, signature },
  },
};
fs.writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`);
NODE

codesign --verify --deep --strict --verbose=2 "$app_bundle"
spctl --assess --type execute --verbose=4 "$app_bundle"
unzip -tq "$human_zip"
unzip -Z1 "$human_zip" | grep '^Noise.app/' >/dev/null
tar -tzf "$updater_archive" | grep '^Noise.app/' >/dev/null
node - "$latest_json" "$version" "$release_url" <<'NODE'
const fs = require("fs");
const [manifestPath, version, url] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const platform = manifest.platforms?.["darwin-aarch64"];
if (manifest.version !== version || platform?.url !== url || !platform.signature) process.exit(1);
NODE

rm -f "$bundle_dir/Noise.app.tar.gz" "$bundle_dir/Noise.app.tar.gz.sig"

printf 'Release assets ready in %s\n' "$assets_dir"
