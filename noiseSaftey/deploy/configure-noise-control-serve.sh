#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "configure-noise-control-serve.sh must run as root" >&2
    exit 1
fi

systemctl is-active --quiet noise-admin-dashboard.service
systemctl is-active --quiet noise-safety-reviewer.service

wait_for_socket() {
    local socket="$1"
    local attempt
    for attempt in {1..50}; do
        if [[ -S "${socket}" ]]; then
            return 0
        fi
        sleep 0.2
    done
    echo "timed out waiting for ${socket}" >&2
    return 1
}

wait_for_socket /run/noise-admin/dashboard.sock
wait_for_socket /run/noise-safety-reviewer/reviewer.sock

probe_headers="$(mktemp /tmp/noise-safety-reviewer-probe.XXXXXXXX)"
trap 'rm -f -- "${probe_headers}"' EXIT

backup="/var/lib/noise-admin/tailscale-serve-before-noise-control.json"
if [[ ! -e "${backup}" ]]; then
    tailscale serve status --json >"${backup}"
    chown root:noise-admin "${backup}"
    chmod 0640 "${backup}"
fi

curl --fail --silent --show-error \
    --unix-socket /run/noise-admin/dashboard.sock \
    --header 'X-Forwarded-Host: cyphers-vps.yakalo-lizard.ts.net:8443' \
    --header 'Tailscale-User-Login: cmcelvogue91@gmail.com' \
    http://localhost/ >/dev/null
curl --fail --silent --show-error \
    --unix-socket /run/noise-safety-reviewer/reviewer.sock \
    --header 'X-Forwarded-Host: cyphers-vps.yakalo-lizard.ts.net:8443' \
    --header 'Tailscale-User-Login: cmcelvogue91@gmail.com' \
    --dump-header "${probe_headers}" \
    http://localhost/ >/dev/null
grep -Fqi 'location: /safety/' "${probe_headers}"

tailscale serve --bg --https=8443 \
    unix:/run/noise-admin/dashboard.sock
tailscale serve --bg --https=8443 --set-path=/safety \
    unix:/run/noise-safety-reviewer/reviewer.sock

tailscale serve status
