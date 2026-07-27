#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
windows_host=${NOISE_WINDOWS_HOST:-noise-windows}
windows_repo=${NOISE_WINDOWS_REPO:-'C:\Users\cmcel\noise'}
revision=${1:-$(git -C "$repo_root" rev-parse HEAD)}
revision=$(git -C "$repo_root" rev-parse "$revision^{commit}")
short_revision=${revision:0:12}
keychain_account=$(id -un)
keychain_service=xyz.gnosyslabs.noise.updater
local_assets="$repo_root/target/release/windows-assets"
temporary_dir=$(mktemp -d /tmp/noise-windows-release.XXXXXX)
remote_script="C:/Users/cmcel/AppData/Local/Temp/noise-release-windows-$short_revision.ps1"
remote_stamp=$(date -u +%Y%m%dT%H%M%SZ)
remote_password='C:\Users\cmcel\AppData\Local\noise-release\updater-password.dpapi'

cleanup() {
  rm -rf "$temporary_dir"
  ssh -n -o BatchMode=yes "$windows_host" \
    "powershell -NoProfile -NonInteractive -Command \"Remove-Item -LiteralPath '$remote_script' -Force -ErrorAction SilentlyContinue\"" \
    >/dev/null 2>&1 || true
}
trap cleanup EXIT

for required_command in git iconv node scp security shasum ssh; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    echo "Missing required command: $required_command" >&2
    exit 1
  fi
done

if ! git -C "$repo_root" diff --quiet || ! git -C "$repo_root" diff --cached --quiet; then
  echo "Commit the Windows release candidate before building it remotely" >&2
  exit 1
fi

git -C "$repo_root" fetch origin main
if ! git -C "$repo_root" merge-base --is-ancestor "$revision" origin/main; then
  echo "Revision $revision is not available from origin/main" >&2
  exit 1
fi

version=$(
  git -C "$repo_root" show "$revision:apps/client/src-tauri/tauri.conf.json" |
    node -e 'let value=""; process.stdin.on("data", chunk => value += chunk); process.stdin.on("end", () => console.log(JSON.parse(value).version));'
)
remote_output="C:\\Users\\cmcel\\AppData\\Local\\noise-release-assets\\$version-$short_revision-$remote_stamp"

ssh -n -o BatchMode=yes -o ConnectTimeout=8 "$windows_host" \
  'powershell -NoProfile -NonInteractive -Command "$env:USERNAME; $env:COMPUTERNAME"' \
  >/dev/null

password_present=$(
  ssh -n -o BatchMode=yes "$windows_host" \
    "powershell -NoProfile -NonInteractive -Command \"Test-Path -LiteralPath '$remote_password' -PathType Leaf\""
)
password_present=$(printf '%s' "$password_present" | tr -d '\r')
if [[ "$password_present" != "True" ]]; then
  updater_password=${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}
  if [[ -z "$updater_password" ]]; then
    updater_password=$(security find-generic-password -a "$keychain_account" -s "$keychain_service" -w)
  fi
  provision_command=$(
    printf '$passwordValues = @($input); if ($passwordValues.Count -ne 1) { throw "Updater password input is invalid" }; $passwordPath = "%s"; $passwordDirectory = Split-Path -Parent $passwordPath; New-Item -ItemType Directory -Force -Path $passwordDirectory | Out-Null; $securePassword = ConvertTo-SecureString -String ([string]$passwordValues[0]) -AsPlainText -Force; $securePassword | ConvertFrom-SecureString | Set-Content -LiteralPath $passwordPath -NoNewline; Write-Output "WINDOWS_UPDATER_PASSWORD_STORED"' \
      "$remote_password"
  )
  provision_encoded=$(
    printf '%s' "$provision_command" |
      iconv -f UTF-8 -t UTF-16LE |
      base64 |
      tr -d '\r\n'
  )
  provision_result=$(
    printf '%s' "$updater_password" |
      ssh -o BatchMode=yes "$windows_host" \
        "powershell -NoProfile -NonInteractive -EncodedCommand $provision_encoded"
  )
  updater_password=
  if ! printf '%s' "$provision_result" | tr -d '\r' |
    grep -Fx 'WINDOWS_UPDATER_PASSWORD_STORED' >/dev/null; then
    echo "Could not provision the DPAPI-protected Windows updater password" >&2
    exit 1
  fi
fi

scp -q "$repo_root/scripts/release-windows.ps1" "$windows_host:$remote_script"

remote_command=$(
  printf "& '%s' -Repository '%s' -Revision '%s' -OutputDirectory '%s' -UpdaterPasswordPath '%s'" \
    "$remote_script" "$windows_repo" "$revision" "$remote_output" \
    "$remote_password"
)
remote_encoded=$(
  printf '%s' "$remote_command" |
    iconv -f UTF-8 -t UTF-16LE |
    base64 |
    tr -d '\r\n'
)
remote_result=$(
  ssh -n -o BatchMode=yes \
    -o ServerAliveInterval=20 \
    -o ServerAliveCountMax=30 \
    "$windows_host" \
    "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $remote_encoded"
)

remote_result=$(printf '%s' "$remote_result" | tr -d '\r')
result_revision=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_REVISION=/{sub(/^[^=]*=/, ""); print; exit}')
result_version=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_VERSION=/{sub(/^[^=]*=/, ""); print; exit}')
remote_installer=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_INSTALLER=/{sub(/^[^=]*=/, ""); print; exit}')
remote_signature=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_SIGNATURE=/{sub(/^[^=]*=/, ""); print; exit}')
remote_installer_hash=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_INSTALLER_SHA256=/{sub(/^[^=]*=/, ""); print; exit}')
remote_signature_hash=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_SIGNATURE_SHA256=/{sub(/^[^=]*=/, ""); print; exit}')
authenticode_status=$(printf '%s\n' "$remote_result" | awk -F= '/^RESULT_AUTHENTICODE_STATUS=/{sub(/^[^=]*=/, ""); print; exit}')

if [[ "$result_revision" != "$revision" || "$result_version" != "$version" ]]; then
  echo "Windows build result did not match the requested release candidate" >&2
  exit 1
fi
if [[ -z "$remote_installer" || -z "$remote_signature" ]]; then
  echo "Windows build did not return both release artifact paths" >&2
  exit 1
fi

remote_installer_scp=${remote_installer//\\//}
remote_signature_scp=${remote_signature//\\//}
scp -q "$windows_host:$remote_installer_scp" "$temporary_dir/"
scp -q "$windows_host:$remote_signature_scp" "$temporary_dir/"

installer_name=$(basename "$remote_installer_scp")
signature_name=$(basename "$remote_signature_scp")
local_installer_hash=$(shasum -a 256 "$temporary_dir/$installer_name" | awk '{print $1}')
local_signature_hash=$(shasum -a 256 "$temporary_dir/$signature_name" | awk '{print $1}')
if [[ "$local_installer_hash" != "$remote_installer_hash" ]]; then
  echo "Windows installer hash changed during transfer" >&2
  exit 1
fi
if [[ "$local_signature_hash" != "$remote_signature_hash" ]]; then
  echo "Windows updater signature hash changed during transfer" >&2
  exit 1
fi

mkdir -p "$local_assets"
mv "$temporary_dir/$installer_name" "$local_assets/$installer_name"
mv "$temporary_dir/$signature_name" "$local_assets/$signature_name"

printf 'Windows release assets ready in %s\n' "$local_assets"
printf 'Revision: %s\n' "$revision"
printf 'Installer SHA-256: %s\n' "$local_installer_hash"
printf 'Updater signature SHA-256: %s\n' "$local_signature_hash"
printf 'Authenticode status: %s\n' "$authenticode_status"
