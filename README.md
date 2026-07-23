# noise

**Noise is a private, group-first messenger built around the people you
actually choose.**

There is no public directory, algorithmic feed, phone number, or email signup.
Create a group, share its secret 12-digit frequency, and the right people can
join. Profiles can have a name, avatar, and bio without attaching the account
to personally identifying information.

[Download the latest alpha for macOS or Windows](https://github.com/GnosysLabs/noise/releases/latest)

## What makes Noise different

- **Groups are the center.** Large private communities are the product, not an
  afterthought bolted onto direct messages.
- **Joining feels intentional.** Groups are found through people, not search or
  recommendations. You join with a frequency shared by someone you trust.
- **No phone number or email.** Your account uses a random Noise ID and a
  password. Your display name can change without changing your identity.
- **Private messages and media.** Message and media payloads are encrypted on
  your device before replaceable relays carry them.
- **Communities have character.** Group icons, backgrounds, accent colors,
  rules, replies, reactions, media galleries, moderation, and roles are built
  into the experience.

Noise is currently an early alpha. It is ready for experimentation and real
communities, but it should not yet be treated as a life-safety tool or a
guarantee of high-risk anonymity.

## How it works

Noise does not have one central server that owns every community.

Clients encrypt content and send signed, padded objects through a network of
replaceable relays. A relay can help mask where a request is going, carry
encrypted group and account state, and store only the opaque media shards
assigned to it. A storage relay sees a request arriving from a mask relay
instead of directly from the client; the mask can forward the request without
reading the encrypted Noise payload.

There is only **one relay program**. “Mask,” “metadata,” and “media storage” are
jobs the same binary can perform for a request, not separate node types an
operator has to understand or maintain.

## Strength in numbers

Every independently operated relay makes Noise harder to erase and less
dependent on any one company or machine.

More relays provide:

- more paths between clients and storage, reducing reliance on a single
  operator;
- more aggregate storage without making every relay host every file;
- better media durability when a machine or provider disappears;
- more geographic and provider diversity; and
- a network that communities can keep alive with inexpensive infrastructure.

Media is encrypted first, then Reed-Solomon encoded and distributed across a
client-selected relay constellation. In a mature 12-relay constellation, any
eight shards can reconstruct the encrypted object. No participating relay
needs the whole object, and no relay mirrors the whole network. The profile
adapts when fewer relays exist.

Independence matters as much as raw count: ten relays run by ten people are more
valuable than ten relays controlled by one provider. Even a small Ubuntu VPS
can make a meaningful contribution.

## Run a relay

The recommended setup is to give the prompt below to a capable coding agent
that can SSH into your Ubuntu VPS. The agent installs a prebuilt, statically
linked package for either x86-64 (`amd64`) or ARM64 (`arm64`). ARM64 here means
Linux servers such as AWS Graviton, Ampere, and many Oracle Cloud machines—not
just Macs.

You do **not** need Docker, Node.js, Rust, a source checkout, or an installer
script hosted by Noise. The package contains one binary and systemd units.
After installation, the relay checks a detached Ed25519-signed release manifest
on a randomized timer. It only installs a package whose exact byte length and
SHA-256 hash were covered by that signature. A failed restart restores the
previous binary and service units, and temporary update files are removed.

### Prompt for your coding agent

Copy this, replace the bracketed values, and give it to an agent with SSH access:

```text
Set up an official Noise relay on my Ubuntu VPS.

SSH host: [HOST OR TAILSCALE IP]
SSH user: [USER]
Public relay domain: [relay.example.com]
Storage: [local disk OR S3-compatible]
Storage quota: [NUMBER OF GB, OR NO APPLICATION QUOTA]

Use the prebuilt noise-relay Debian package from GnosysLabs/noise. Do not use
Docker, do not build from source, and do not run a curl-piped installer.

Before trusting any download URL, fetch these three files directly from the
GnosysLabs/noise repository:
- deploy/relay-channels/stable.json
- deploy/relay-channels/stable.json.sig
- deploy/relay-release-public.pem

Verify the detached Ed25519 signature over the exact stable.json bytes with
OpenSSL. Read the signed manifest only after verification. Detect whether the
VPS uses amd64 or arm64, download the matching .deb from the URL in that
manifest, and verify both its exact byte length and SHA-256 hash before
installing it with apt.

Configure /etc/noise-relay/config.toml with:
- listen = 127.0.0.1:4301
- data = /var/lib/noise-relay
- public_url = the HTTPS domain above
- both official bootstrap relays from the example config
- both official mask targets from the example config
- the requested storage quota

If S3-compatible storage was selected, put its credentials in
/etc/noise-relay/storage.env with mode 600. Never print the secrets. Otherwise
use local storage and do not create that file.

Set up the public domain with a normal HTTPS reverse proxy from port 443 to
127.0.0.1:4301 using a distro package already available for Ubuntu. Preserve
any unrelated web services and firewall rules. Enable and start
noise-relay.service and noise-relay-update.timer.

Finally run:
- noise-relay --config /etc/noise-relay/config.toml status
- noise-relay --config /etc/noise-relay/config.toml doctor
- systemctl status noise-relay.service --no-pager
- systemctl status noise-relay-update.timer --no-pager

Confirm the public /health endpoint reports the relay software and protocol
versions, the signed public relay descriptor verifies, the durable data
directory is owned by noise-relay, and the service survives one restart.
Report exactly what was installed and any step that still needs my input.
```

### Storage choices

Local storage is the default. Encrypted shards live under
`/var/lib/noise-relay/shards`, while signed indexes and small deletion records
live in the relay's embedded, self-hosted Turso database. Media bytes are not
stored in Turso and are not loaded into RAM when the relay starts.

For an S3-compatible bucket, copy
[`deploy/storage.env.example`](deploy/storage.env.example) to
`/etc/noise-relay/storage.env`, fill it in, and set its mode to `600`. Amazon
S3, Cloudflare R2, Backblaze B2, MinIO, and compatible providers can be used.
The bucket receives only the opaque shards assigned to this relay—not a copy of
the network and not plaintext media.

The main relay configuration starts from
[`deploy/noise-relay.toml.example`](deploy/noise-relay.toml.example). Package
installation preserves operator changes to `/etc/noise-relay/config.toml`.

### Operator commands

```sh
# Configuration and version, formatted for a human
noise-relay --config /etc/noise-relay/config.toml status

# The same status as JSON for an agent or monitoring system
noise-relay --config /etc/noise-relay/config.toml status --json

# Local health, public reachability, signed identity, and durable data
noise-relay --config /etc/noise-relay/config.toml doctor

# Check the signed stable channel without installing anything
noise-relay update

# Follow the service
journalctl -u noise-relay.service -f
```

Operators remain in control of their machines. Noise can offer signed updates,
but no central authority can force an independently operated relay to install
one.

## Development

The repository is a Rust workspace with one React interface shared by Tauri on
macOS and Windows:

- `apps/client`: desktop interface and Tauri shell
- `apps/marketing`: public site for `makenoise.chat`
- `noise-core`: identity, groups, encryption, signed events, and media coding
- `noise-client`: reusable profile, group, DM, and moderation operations
- `noise-transport`: padded Binary HTTP and oblivious relay transport
- `noise-relay`: the single relay binary
- `noise-cli`: protocol exercise and debugging client
- `noise-sim`: signed-event membership scale simulator

Run the desktop app:

```sh
cd apps/client
pnpm install
pnpm tauri dev
```

Run two local relays:

```sh
cargo run -p noise-relay -- \
  --listen 127.0.0.1:4301 \
  --public-url http://127.0.0.1:4301 \
  --mask-target http://127.0.0.1:4302

cargo run -p noise-relay -- \
  --listen 127.0.0.1:4302 \
  --public-url http://127.0.0.1:4302 \
  --mask-target http://127.0.0.1:4301
```

Each startup prints a shareable address containing the relay's pinned OHTTP
public key. The fragment is not sent in an HTTP request.

Protocol details live in [`docs/PROTOCOL.md`](docs/PROTOCOL.md), client notes in
[`docs/CLIENTS.md`](docs/CLIENTS.md), and the 50,000-member reducer benchmark in
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). The signed relay publication
process is documented in
[`docs/RELAY_RELEASES.md`](docs/RELAY_RELEASES.md).
