#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "configure-noise-control-serve.sh must run as root" >&2
    exit 1
fi

systemctl is-active --quiet noise-admin-dashboard.service
systemctl is-active --quiet noise-safety-reviewer.service
[[ -S /run/noise-admin/dashboard.sock ]]
[[ -S /run/noise-safety-reviewer/reviewer.sock ]]

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
    http://localhost/safety/ >/dev/null

tailscale serve --bg --https=8443 \
    unix:/run/noise-admin/dashboard.sock
tailscale serve --bg --https=8443 --set-path=/safety \
    unix:/run/noise-safety-reviewer/reviewer.sock

tailscale serve status
