#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <relay-binary> <amd64|arm64> <version> <output-directory>" >&2
  exit 2
fi

relay_binary=$1
package_arch=$2
package_version=$3
output_directory=$4
script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_directory/.." && pwd)

if [[ ! -x "$relay_binary" ]]; then
  echo "relay binary is missing or not executable: $relay_binary" >&2
  exit 2
fi
if [[ "$package_arch" != "amd64" && "$package_arch" != "arm64" ]]; then
  echo "unsupported Debian architecture: $package_arch" >&2
  exit 2
fi
if [[ ! "$package_version" =~ ^[0-9]+(\.[0-9]+){2}([+~-][0-9A-Za-z.+~-]+)?$ ]]; then
  echo "invalid Debian package version: $package_version" >&2
  exit 2
fi

package_root=$(mktemp -d)
cleanup() {
  rm -rf -- "$package_root"
}
trap cleanup EXIT

install -Dm755 "$relay_binary" "$package_root/usr/bin/noise-relay"
install -Dm644 \
  "$repository_root/deploy/noise-relay.toml.example" \
  "$package_root/etc/noise-relay/config.toml"
install -Dm644 \
  "$repository_root/deploy/systemd/noise-relay.service" \
  "$package_root/lib/systemd/system/noise-relay.service"
install -Dm644 \
  "$repository_root/deploy/systemd/noise-relay-update.service" \
  "$package_root/lib/systemd/system/noise-relay-update.service"
install -Dm644 \
  "$repository_root/deploy/systemd/noise-relay-update.timer" \
  "$package_root/lib/systemd/system/noise-relay-update.timer"
install -Dm644 \
  "$repository_root/deploy/storage.env.example" \
  "$package_root/usr/share/doc/noise-relay/storage.env.example"
install -Dm644 \
  "$repository_root/deploy/relay-release-public.pem" \
  "$package_root/usr/share/doc/noise-relay/relay-release-public.pem"

mkdir -p "$package_root/DEBIAN"
cat >"$package_root/DEBIAN/control" <<EOF
Package: noise-relay
Version: $package_version
Section: net
Priority: optional
Architecture: $package_arch
Maintainer: Gnosys Labs <noise@gnosyslabs.xyz>
Depends: ca-certificates, systemd
Description: private group messaging relay for Noise
 A single untrusted relay binary that masks requests, replicates signed
 encrypted metadata, and contributes bounded local or S3-compatible shard
 storage to the Noise network.
EOF

cat >"$package_root/DEBIAN/conffiles" <<'EOF'
/etc/noise-relay/config.toml
EOF

cat >"$package_root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e

if ! getent group noise-relay >/dev/null 2>&1; then
  addgroup --system noise-relay >/dev/null
fi
if ! id noise-relay >/dev/null 2>&1; then
  adduser \
    --system \
    --ingroup noise-relay \
    --home /var/lib/noise-relay \
    --no-create-home \
    --disabled-login \
    --disabled-password \
    noise-relay >/dev/null
fi

install -d -m 0700 -o noise-relay -g noise-relay /var/lib/noise-relay
install -d -m 0755 -o root -g root /etc/noise-relay

systemctl daemon-reload >/dev/null 2>&1 || true
systemctl enable noise-relay.service >/dev/null 2>&1 || true
systemctl enable --now noise-relay-update.timer >/dev/null 2>&1 || true
if systemctl is-active --quiet noise-relay.service; then
  systemctl try-restart noise-relay.service >/dev/null 2>&1 || true
fi
EOF

cat >"$package_root/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e

if [ "${1:-}" = "remove" ]; then
  systemctl disable --now noise-relay-update.timer >/dev/null 2>&1 || true
  systemctl disable --now noise-relay.service >/dev/null 2>&1 || true
fi
EOF

cat >"$package_root/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e

systemctl daemon-reload >/dev/null 2>&1 || true
EOF

chmod 0755 \
  "$package_root/DEBIAN/postinst" \
  "$package_root/DEBIAN/prerm" \
  "$package_root/DEBIAN/postrm"

mkdir -p "$output_directory"
package_path="$output_directory/noise-relay_${package_version}_${package_arch}.deb"
dpkg-deb --root-owner-group -Zgzip -z6 --build "$package_root" "$package_path"
echo "$package_path"
