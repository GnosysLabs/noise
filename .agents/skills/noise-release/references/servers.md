# noise production servers

This topology was verified on 2026-07-25. Treat it as a starting map, not timeless truth: run the read-only discovery commands before every deployment.

## Connection map

| Role | SSH command | Login | Public service |
|---|---|---|---|
| Primary web/APNs relay | `ssh root@cyphers-vps` | `root` over Tailscale MagicDNS | `app.makenoise.chat`, `noiserelay.gnosyslabs.xyz` |
| Second privacy relay | `ssh I2P` | `admin` from `~/.ssh/config`; use `sudo` for system files | `noiserelay.irisirc.chat` |

For Cyphers VPS, use `root@cyphers-vps`, not `chat.gnosyslabs.xyz`. The latter is a public web DNS name and bypasses the intended Tailscale path.

The local SSH config entry is written as `Host I2P VPS`, which makes both `I2P` and `VPS` matching aliases. Use the unambiguous `ssh I2P`.

If a host key changes, first verify the Tailscale/public host identity. Never blindly delete a known-host entry.

## Read-only preflight

Primary:

```sh
ssh root@cyphers-vps '
  hostname
  systemctl status noise-relay.service --no-pager -l
  systemctl status noise-relay-update.timer --no-pager -l
  systemctl cat noise-relay.service
  stat -c "%U:%G %a %n" /usr/bin/noise-relay /etc/noise-relay/config.toml
  curl -fsS http://127.0.0.1:4301/health
'
curl -fsS https://noiserelay.gnosyslabs.xyz/health
curl -fsS https://app.makenoise.chat/
```

Second relay:

```sh
ssh I2P '
  hostname
  systemctl status noise-relay.service --no-pager -l
  systemctl status noise-relay-update.timer --no-pager -l
  systemctl cat noise-relay.service
  sudo -n stat -c "%U:%G %a %n" /usr/bin/noise-relay /etc/noise-relay/config.toml
  curl -fsS http://127.0.0.1:4301/health
'
curl -fsS https://noiserelay.irisirc.chat/health
```

Also inspect recent logs:

```sh
ssh root@cyphers-vps 'journalctl -u noise-relay.service -n 100 --no-pager'
ssh I2P 'sudo journalctl -u noise-relay.service -n 100 --no-pager'
```

Do not dump complete configs when they may contain secret paths or credentials. Select only the non-secret keys needed for diagnosis.

## Primary Cyphers layout

- Web root: `/var/www/app.makenoise.chat`
- Web nginx site: `/etc/nginx/sites-available/app.makenoise.chat`
- Relay binary: `/usr/bin/noise-relay`
- Relay data: `/var/lib/noise-relay`
- Relay config: `/etc/noise-relay/config.toml`
- Relay service: `noise-relay.service`
- Relay update timer: `noise-relay-update.timer`
- Loopback listener: `127.0.0.1:4301`
- Public relay: `https://noiserelay.gnosyslabs.xyz`
- Service account: `noise-relay`

This is the relay that currently has APNs delivery configured. Preserve `/etc/noise-relay/AuthKey_HFYLVC3UAG.p8`; never display, download, or overwrite it during an ordinary deployment.

## I2P VPS layout

- Relay binary: `/usr/bin/noise-relay`
- Relay data: `/var/lib/noise-relay`
- Relay config: `/etc/noise-relay/config.toml`
- Relay service: `noise-relay.service`
- Relay update timer: `noise-relay-update.timer`
- Loopback listener: `127.0.0.1:4301`
- Public relay: `https://noiserelay.irisirc.chat`
- Service account: `noise-relay`
- Administrative login: `admin`, with `sudo`

The two relays mask through each other. Keep both public health endpoints available and protocol-compatible.

## Web deployment on Cyphers

Build locally first with the canonical web build. After explicit production authorization:

```sh
release_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
ssh root@cyphers-vps \
  "cp -a /var/www/app.makenoise.chat /var/www/app.makenoise.chat.before-${release_stamp}"
rsync -az --delete \
  /Users/christopher/Dev/noise/apps/client/dist/ \
  root@cyphers-vps:/var/www/app.makenoise.chat/
```

Before `rsync --delete`, validate that the local source ends in `apps/client/dist/` and the remote target is exactly `/var/www/app.makenoise.chat/`.

After syncing, verify the public `index.html` and every content-hashed JS/WASM asset it references. If rollback is required:

1. Move the failed web root to a distinct explicit path.
2. Restore the recorded `before-<timestamp>` directory to `/var/www/app.makenoise.chat`.
3. Verify production again.

Do not edit nginx or certificates during a normal static-client deployment.

## Direct relay binary deployment

Prefer the signed relay release channel. Use this direct path only for an explicitly authorized hotfix.

Build once from the accepted source tree:

```sh
cargo zigbuild --release -p noise-relay --target x86_64-unknown-linux-gnu.2.35
shasum -a 256 target/x86_64-unknown-linux-gnu/release/noise-relay
```

Stage to both hosts without replacing the running binary:

```sh
scp target/x86_64-unknown-linux-gnu/release/noise-relay \
  root@cyphers-vps:/tmp/noise-relay.new
scp target/x86_64-unknown-linux-gnu/release/noise-relay \
  I2P:/tmp/noise-relay.new
```

On each host, verify the staged SHA-256 and run `ldd /tmp/noise-relay.new`; require no `not found`. Then back up and install with a single explicit stamp.

Primary:

```sh
deploy_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
ssh root@cyphers-vps "
  cp -a /usr/bin/noise-relay /usr/bin/noise-relay.before-${deploy_stamp} &&
  cp -a /etc/noise-relay/config.toml /etc/noise-relay/config.toml.before-${deploy_stamp} &&
  install -o root -g root -m 0755 /tmp/noise-relay.new /usr/bin/noise-relay &&
  systemctl restart noise-relay.service &&
  systemctl is-active noise-relay.service
"
```

Second relay:

```sh
deploy_stamp="$(date -u +%Y%m%dT%H%M%SZ)"
ssh I2P "
  sudo cp -a /usr/bin/noise-relay /usr/bin/noise-relay.before-${deploy_stamp} &&
  sudo cp -a /etc/noise-relay/config.toml /etc/noise-relay/config.toml.before-${deploy_stamp} &&
  sudo install -o root -g root -m 0755 /tmp/noise-relay.new /usr/bin/noise-relay &&
  sudo systemctl restart noise-relay.service &&
  sudo systemctl is-active noise-relay.service
"
```

Deploy and verify one relay before replacing the other so one healthy transport remains available. Do not alter the existing unit arguments or config as part of a binary-only deployment.

After each restart, verify local health, public health, software/protocol versions, and recent logs. Remove the staged `/tmp/noise-relay.new` only after acceptance.

## Direct relay rollback

Use the exact backup stamp recorded for that host:

```sh
ssh root@cyphers-vps '
  install -o root -g root -m 0755 /usr/bin/noise-relay.before-<stamp> /usr/bin/noise-relay &&
  cp -a /etc/noise-relay/config.toml.before-<stamp> /etc/noise-relay/config.toml &&
  systemctl restart noise-relay.service
'
```

Use the corresponding `sudo` form through `ssh I2P` for the second relay. Re-run local/public health and client behavior after rollback.

## Managed relay deployment

Both servers have `noise-relay-update.timer` enabled. The preferred production flow is:

1. Publish signed amd64/arm64 packages and the signed stable channel.
2. Commit/push the exact channel manifest and signature.
3. Allow the timers to fetch and cryptographically verify the update.
4. Watch both timer/service journals.
5. Confirm both public `/health` responses report the intended software and protocol versions.

Do not manually force both updater services simultaneously. Update/observe one fault domain at a time when possible.
