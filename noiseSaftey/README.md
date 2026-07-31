# noise safety intake

This is the separate, write-only intake boundary for reports sent to noise
safety. It accepts only HPKE-sealed `SealedSafetyReportV1` envelopes and stores
them without decrypting, previewing, emailing, or logging report contents.

The same binary provides the public intake and the private reviewer.
Production keeps those roles under separate Unix accounts and systemd
sandboxes. The reviewer listens only on a mode-`0600` Unix socket consumed by
Tailscale Serve; it has no public or loopback TCP listener.

Generate separate public and private recipient-key files:

```sh
cargo run -p noise-safety-intake -- keygen \
  --public-output noiseSaftey/dev-data/recipient-public.json \
  --secret-output noiseSaftey/dev-data/recipient-secret.json
```

Run the public intake with only the public file:

```sh
cargo run -p noise-safety-intake -- serve \
  --public-key-file noiseSaftey/dev-data/recipient-public.json \
  --spool-dir noiseSaftey/dev-data/inbox \
  --directive-dir noiseSaftey/dev-data/inbox/.review-state/directive-outbox
```

Run the private reviewer on a separate loopback port:

```sh
cargo run -p noise-safety-intake -- review \
  --secret-key-file noiseSaftey/dev-data/recipient-secret.json \
  --spool-dir noiseSaftey/dev-data/inbox
```

The generated **noise control** launcher runs
`noiseSaftey/open-reviewer.exp` and opens the production tailnet URL. The
browser must be on the noise Tailscale network. Tailscale supplies the
authenticated login, and the reviewer accepts only the exact accounts listed
by repeated `--tailscale-login` arguments. There is no separate reviewer
password and no public fallback URL.

The same tailnet URL also hosts **noise control**, a read-only operational
dashboard with Overview, Usage, Infrastructure, Safety, and Audit Log
navigation. The stable root is served by a separate `noise-admin` process with
an independently provisioned PostgreSQL role. `/safety` is routed to this
reviewer process; report decryption keys and directive signing keys never enter
the dashboard process. Tailscale Serve strips the `/safety` mount prefix before
forwarding requests, while the reviewer adds that prefix back to generated
browser links and redirects.

After Tailscale authentication, the `/safety` URL redirects to a random
single-process capability path. The reviewer records the authenticated
Tailscale login with each immutable decision. It decrypts and
cryptographically verifies reports only inside the isolated reviewer service.
New reports include encrypted human context: names displayed at report time,
noise signatures, the cryptographically verifiable founder, and a
reporter-signed moderator snapshot. Reports created before that context was
added still show noise signatures, but their names remain unavailable.

The reviewer never renders or retrieves reported media. A reviewer may
deliberately download the complete decrypted and verified report as JSON; that
file can contain reported message text and private metadata, but never media
bytes or decryption keys.

Each report can be closed with no action or produce one of these signed,
content-free directives:

- suppress the reported event;
- restrict the group for 24 hours;
- restrict the group indefinitely;
- restrict the reported identity indefinitely.

The directive protocol also supports signed group and identity restores.
Temporary and indefinite group restrictions use the same client enforcement
mechanism; only the optional expiry differs.

Local decisions are stored under `.review-state/decisions`. Enforcement
directives are reconciled into `.review-state/directive-outbox` so a crash
between the decision and outbox write cannot lose an action. The directive
signer is deterministically derived from the existing recipient secret with a
separate KDF context, preserving compatibility with existing encrypted inbox
files.

The public service exposes only verified, content-free files from the outbox at
`GET /v1/directives`. Official clients verify every item with a pinned signing
key, merge it into durable last-known local state, hide suppressed events and
restricted identities, and replace restricted groups with a neutral unavailable
screen. An empty or truncated feed never erases a decision already learned by a
client. Expired temporary restrictions stop applying locally, while indefinite
restrictions remain until a newer signed restore arrives.

During development, the intake and reviewer can share this local directory.
The production reviewer remains unreachable from the public internet. A
root-owned bridge copies only envelopes verified by the public intake into the
reviewer's private inbox and installs only correctly signed, content-free
directives in the public feed. The public intake never receives or mounts the
private recipient key.

The client development build uses `http://127.0.0.1:4310`. A production build
must configure both `VITE_NOISE_SAFETY_URL` and the pinned
`VITE_NOISE_SAFETY_PUBLIC_KEY` for reports, plus
`VITE_NOISE_SAFETY_DIRECTIVE_SIGNING_PUBLIC_KEY` for enforcement. It will not
trust keys fetched from a remote intake.

## Review and enforcement workflow

1. Official clients send only an HPKE-sealed envelope to the public intake.
2. A root-owned local bridge copies the still-encrypted, verified envelope into
   the isolated reviewer inbox.
3. An allowlisted Tailscale user opens the private web reviewer. The reviewer
   decrypts and verifies a case without downloading or rendering reported
   media.
4. **No action** closes the case without publishing anything. **Hide message**,
   **Pause group for 24 hours**, **Block group**, and **Block author** create a
   signed, content-free directive.
5. The bridge cryptographically verifies each signed directive and installs it
   in the public directive directory.
6. Official clients verify the public feed with their pinned key, enforce the
   directive, and purge affected decrypted caches. A failed or interrupted
   purge remains pending and retries on the next safety sync.

The full client behavior, cache policy, restoration behavior, and limits are
documented in [`docs/SAFETY.md`](../docs/SAFETY.md).

## Production boundary

The public service at `safety.makenoise.chat` runs only the `serve` command as
the unprivileged `noise-safety` user. Its deployment uses:

- `/etc/noise-safety/recipient-public.json` — public encryption and directive
  verification keys;
- `/var/lib/noise-safety/inbox` — encrypted report envelopes;
- `/var/lib/noise-safety/directives` — signed, content-free public decisions;
- `noiseSaftey/deploy/noise-safety-intake.service` — the sandboxed systemd unit;
- `noiseSaftey/deploy/safety.makenoise.chat.nginx` — the HTTPS proxy source.

The production recipient secret is stored mode `0600` under the separate
`noise-safety-reviewer` account. The public `noise-safety` account cannot read
that key or the decrypted reviewer state. Tailscale Serve connects directly to
the reviewer's private Unix socket, and the application independently checks
the exact `Tailscale-User-Login` allowlist before showing any report.

The online reviewer deployment uses:

- `/etc/noise-safety-reviewer/recipient-secret.json` — private recipient key;
- `/var/lib/noise-safety-reviewer/inbox` — private encrypted-envelope mirror;
- `/var/lib/noise-safety-reviewer/state` — decisions and signed outbox;
- `noiseSaftey/deploy/noise-safety-reviewer.service` — isolated reviewer;
- `noiseSaftey/deploy/noise-safety-reviewer-sync.timer` — ten-second bridge;
- `https://cyphers-vps.yakalo-lizard.ts.net:8443/` — tailnet-only reviewer
  and admin URL. Port 8443 avoids the VPS's public nginx listener on 443.

The private admin deployment additionally uses:

- `/etc/noise-admin/environment` — mode-`0640` credentials for the restricted
  `noise_admin` PostgreSQL role;
- `/run/noise-admin/dashboard.sock` — private dashboard Unix socket;
- `noiseSaftey/deploy/bootstrap-noise-admin.sh` — idempotent service-user and
  read-only database-role provisioning;
- `noiseSaftey/deploy/noise-admin-dashboard.service` — sandboxed dashboard;
- `noiseSaftey/deploy/configure-noise-control-serve.sh` — Tailscale Serve path
  routing for `/` and `/safety`.

The dashboard reads aggregate operational metadata only. It never receives
message text, media plaintext, passwords, private identity keys, stable IP
histories, report plaintext, or report decryption keys. Its database role has
`SELECT` access only to three aggregate views—not the underlying production
tables—defaults to read-only transactions, and has a five-second statement
timeout.

The restricted Mac sync remains available for emergency encrypted export; it
does not participate in normal online reviewing and does not use the Mac's
root-capable VPS key. The server has a separate `noise-safety-sync` SSH account
whose key is forced through
`noiseSaftey/deploy/noise-safety-sync-gateway`. That gateway accepts only
`list`, `read <receipt-id>`, and `install <directive-id>`. The first two expose
only verified HPKE envelopes; the last accepts only directives whose signature
matches the public key pinned by the intake. SSH disables PTY, forwarding,
agent forwarding, X11 forwarding, and user commands for this key.
