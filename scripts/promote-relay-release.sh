#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 3 ]]; then
  echo "usage: $0 <relay-vX.Y.Z> [stable|canary] [release-private-key]" >&2
  exit 2
fi

release_tag=$1
release_channel=${2:-stable}
release_key=${3:-${NOISE_RELAY_RELEASE_KEY:-$HOME/.config/noise/relay-release.pem}}
repository=${NOISE_GITHUB_REPOSITORY:-GnosysLabs/noise}
script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_directory/.." && pwd)
manifest_directory="$repository_root/deploy/relay-channels"

if [[ "$release_channel" != "stable" && "$release_channel" != "canary" ]]; then
  echo "release channel must be stable or canary" >&2
  exit 2
fi
if [[ ! "$release_tag" =~ ^relay-v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  echo "relay tag must look like relay-v0.1.0" >&2
  exit 2
fi
release_version=${BASH_REMATCH[1]}
if [[ ! -f "$release_key" ]]; then
  echo "relay release private key not found: $release_key" >&2
  exit 2
fi
if stat -f '%Lp' "$release_key" >/dev/null 2>&1; then
  release_key_mode=$(stat -f '%Lp' "$release_key")
else
  release_key_mode=$(stat -c '%a' "$release_key")
fi
if [[ "$release_key_mode" != "600" ]]; then
  echo "relay release private key must have mode 600" >&2
  exit 2
fi

package_version=$(awk -F '"' '/^version = / { print $2; exit }' \
  "$repository_root/crates/noise-relay/Cargo.toml")
if [[ "$release_version" != "$package_version" ]]; then
  echo "tag $release_tag does not match noise-relay version $package_version" >&2
  exit 2
fi

release_is_draft=$(gh release view "$release_tag" \
  --repo "$repository" \
  --json isDraft \
  --jq .isDraft)
if [[ "$release_is_draft" != "true" ]]; then
  echo "$release_tag must still be a draft release" >&2
  exit 2
fi

working_directory=$(mktemp -d)
cleanup() {
  rm -rf -- "$working_directory"
}
trap cleanup EXIT

gh release download "$release_tag" \
  --repo "$repository" \
  --pattern "noise-relay_${release_version}_*.deb" \
  --dir "$working_directory"

amd64_package="$working_directory/noise-relay_${release_version}_amd64.deb"
arm64_package="$working_directory/noise-relay_${release_version}_arm64.deb"
test -f "$amd64_package"
test -f "$arm64_package"

protocol_version=$(sed -nE \
  's/^pub const RELAY_PROTOCOL_VERSION: u16 = ([0-9]+);/\1/p' \
  "$repository_root/crates/noise-transport/src/lib.rs")
if [[ -z "$protocol_version" ]]; then
  echo "could not read relay protocol version" >&2
  exit 2
fi

mkdir -p "$manifest_directory"
manifest_path="$manifest_directory/${release_channel}.json"
signature_path="${manifest_path}.sig"
published_at=$(date +%s)
expires_at=$((published_at + 90 * 24 * 60 * 60))

RELEASE_CHANNEL="$release_channel" \
RELEASE_VERSION="$release_version" \
RELEASE_TAG="$release_tag" \
PROTOCOL_VERSION="$protocol_version" \
PUBLISHED_AT="$published_at" \
EXPIRES_AT="$expires_at" \
REPOSITORY="$repository" \
AMD64_PACKAGE="$amd64_package" \
ARM64_PACKAGE="$arm64_package" \
node >"$manifest_path" <<'NODE'
const { createHash } = require("node:crypto");
const { readFileSync, statSync } = require("node:fs");
const { basename } = require("node:path");

function asset(target, path) {
  const bytes = readFileSync(path);
  const file = basename(path);
  return {
    target,
    url: `https://github.com/${process.env.REPOSITORY}/releases/download/${process.env.RELEASE_TAG}/${file}`,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    byte_length: statSync(path).size,
  };
}

const manifest = {
  schema: 1,
  channel: process.env.RELEASE_CHANNEL,
  version: process.env.RELEASE_VERSION,
  protocol_min: Number(process.env.PROTOCOL_VERSION),
  protocol_max: Number(process.env.PROTOCOL_VERSION),
  published_at_unix_seconds: Number(process.env.PUBLISHED_AT),
  expires_at_unix_seconds: Number(process.env.EXPIRES_AT),
  assets: [
    asset("linux-x86_64", process.env.AMD64_PACKAGE),
    asset("linux-aarch64", process.env.ARM64_PACKAGE),
  ],
};

process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
NODE

signature_binary="$working_directory/manifest.sig"
openssl pkeyutl \
  -sign \
  -rawin \
  -inkey "$release_key" \
  -in "$manifest_path" \
  -out "$signature_binary"
openssl base64 -A -in "$signature_binary" >"$signature_path"
printf '\n' >>"$signature_path"
openssl pkeyutl \
  -verify \
  -pubin \
  -inkey "$repository_root/deploy/relay-release-public.pem" \
  -rawin \
  -in "$manifest_path" \
  -sigfile "$signature_binary" \
  >/dev/null

gh release upload "$release_tag" \
  "$manifest_path" \
  "$signature_path" \
  --clobber \
  --repo "$repository"
gh release edit "$release_tag" \
  --draft=false \
  --latest=false \
  --repo "$repository"

echo "Published $release_tag without replacing the desktop app's Latest release."
echo "Signed channel files are ready to commit:"
echo "  $manifest_path"
echo "  $signature_path"
