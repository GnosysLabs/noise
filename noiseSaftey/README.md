# noise safety intake

This is the separate, write-only intake boundary for reports sent to noise
safety. It accepts only HPKE-sealed `SealedSafetyReportV1` envelopes and stores
them without decrypting, previewing, emailing, or logging report contents.

The same binary provides the public intake and the private localhost-only
reviewer. Production keeps those roles on separate machines.

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

On the reviewer Mac, the generated **noise safety reviewer** launcher runs
`noiseSaftey/open-reviewer.exp` in Terminal. Before opening the private
one-time URL, it uses the dedicated restricted sync key to pull verified
encrypted envelopes from production. While the reviewer is open, it checks
again every ten seconds and uploads only correctly signed, content-free
directives from the local outbox. Keep that Terminal window open while
reviewing; closing it stops both the reviewer and its sync loop. Clicking the
launcher again reopens the same private session instead of starting a second
reviewer.

The reviewer prints a random, single-session localhost URL. It decrypts and
cryptographically verifies reports only inside that private service. New
reports include encrypted human context: names displayed at report time, noise
signatures, the cryptographically verifiable founder, and a reporter-signed
moderator snapshot. Reports created before that context was added still show
noise signatures, but their names remain unavailable.

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

During development, the intake and reviewer can share this local directory. A
production reviewer must remain unreachable from the internet and obtain only
encrypted envelopes through an outbound pull or another authenticated private
transfer. The public intake must never receive or mount the private recipient
key.

The client development build uses `http://127.0.0.1:4310`. A production build
must configure both `VITE_NOISE_SAFETY_URL` and the pinned
`VITE_NOISE_SAFETY_PUBLIC_KEY` for reports, plus
`VITE_NOISE_SAFETY_DIRECTIVE_SIGNING_PUBLIC_KEY` for enforcement. It will not
trust keys fetched from a remote intake.

## Review and enforcement workflow

1. Official clients send only an HPKE-sealed envelope to the public intake.
2. The production reviewer Mac pulls the still-encrypted inbox files over SSH.
3. The localhost reviewer decrypts and verifies a case. It never downloads or
   renders reported media.
4. **No action** closes the case without publishing anything. **Hide message**,
   **Pause group for 24 hours**, **Block group**, and **Block author** create a
   signed, content-free directive.
5. The signed directive JSON is uploaded to the public directive directory.
   The private report and reviewer key stay on the reviewer Mac.
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

The production recipient secret remains on the reviewer Mac. Encrypted inbox
files are pulled over SSH for local review, and signed directive JSON files are
uploaded after a decision. The public VPS does not receive the reviewer secret
or a reviewer dashboard.

Production sync does not use the Mac's root-capable VPS key. The server has a
separate `noise-safety-sync` SSH account whose key is forced through
`noiseSaftey/deploy/noise-safety-sync-gateway`. That gateway accepts only
`list`, `read <receipt-id>`, and `install <directive-id>`. The first two expose
only verified HPKE envelopes; the last accepts only directives whose signature
matches the public key pinned by the intake. SSH disables PTY, forwarding,
agent forwarding, X11 forwarding, and user commands for this key.
