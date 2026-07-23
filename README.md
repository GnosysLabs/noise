# noise

Noise is a group-first messaging protocol. Groups are joined through numeric
frequencies and replicated across replaceable relays. Accounts use a random
12-digit Noise ID, a strong password, and a freely changeable display name—no
phone number, email address, or other personally identifying information.

The primary client is one React interface shared by Tauri on macOS and Windows
and, as the browser adapter comes online, the web. Tauri calls the shared Rust
client directly; the earlier native macOS client remains as a design reference.

## Workspace

- `noise-core`: identity, frequency, invitation, and signed-event primitives
- `noise-client`: reusable profile, group, and conversation operations
- `noise-transport`: padded Binary HTTP and RFC 9458 oblivious relay transport
- `noise-ffi`: narrow native bridge into the shared client
- `noise-relay`: one untrusted relay binary for masking, metadata, and shard storage
- `noise`: a small CLI that exercises the protocol end to end
- `noise-sim`: a real signed-event membership scale simulator
- `apps/client`: shared React interface and Tauri desktop shell
- `apps/macos`: earlier native SwiftUI macOS client, kept as a reference

## Shared desktop client

The app currently expects the three development relays below to be running. It
reuses the same identity file as the native client. Run it with:

```sh
cd apps/client
pnpm install
pnpm tauri dev
```

Build the shared browser interface with `pnpm build`. The browser currently
shows a foundation screen until the Rust protocol core, IndexedDB identity
store, and browser transport adapter are connected; see
[`docs/CLIENTS.md`](docs/CLIENTS.md).

Clients must point at relays they can both reach. Two or more production relays
use pinned private relay descriptors so each storage request travels through a
different mask relay:

```sh
VITE_NOISE_RELAYS='https://RELAY_ONE#ohttp=KEY,https://RELAY_TWO#ohttp=KEY' \
  pnpm tauri build --debug
```

Localhost remains the default for development. Relay data is durable, but the
three older development processes must be restarted on the current build before
they begin writing their existing in-memory state to disk.

## Native macOS client

The app currently expects the three development relays below to be running.
Generate the Xcode project and build it with:

```sh
cd apps/macos
xcodegen generate
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer xcodebuild \
  -project Noise.xcodeproj \
  -scheme Noise \
  -configuration Debug \
  -derivedDataPath .derived \
  CODE_SIGNING_ALLOWED=NO \
  build
open .derived/Build/Products/Debug/noise.app
```

The Xcode build phase compiles and links `noise-ffi`; the UI never shells out to
the CLI and does not contain a web view. Local identity state is stored at
`~/Library/Application Support/noise/profile.json` with private file permissions.

## Local demonstration

Start three relays in separate terminals. Each relay explicitly allowlists the
other two as privacy-mask destinations, preventing it from becoming an open
proxy:

```sh
cargo run -p noise-relay -- --listen 127.0.0.1:4301 --public-url http://127.0.0.1:4301 --mask-target http://127.0.0.1:4302 --mask-target http://127.0.0.1:4303
cargo run -p noise-relay -- --listen 127.0.0.1:4302 --public-url http://127.0.0.1:4302 --mask-target http://127.0.0.1:4301 --mask-target http://127.0.0.1:4303
cargo run -p noise-relay -- --listen 127.0.0.1:4303 --public-url http://127.0.0.1:4303 --mask-target http://127.0.0.1:4301 --mask-target http://127.0.0.1:4302
```

Each startup prints a shareable address containing the relay's pinned OHTTP
public key. The fragment is never sent in an HTTP request. Pass at least two of
those complete addresses to the CLI or desktop build. The mask sees the client
IP and destination relay but only padded ciphertext; the storage relay sees the
decrypted Noise request coming from the mask, not the client connection. Masks
and storage relays must be operated independently for that separation to mean
anything.

There is one relay program and one relay protocol. Every `noise-relay` can mask
traffic, replicate signed account/group metadata, and contribute media storage.
“Mask” and “storage” describe a relay's role for one private request; they are
not different server types or binaries.

Small indexes, signed events, invitations, and deletion records live in an
embedded, self-hosted Turso database under `relay-data/<port>` by default.
Media never does. The client encrypts each 1 MiB media chunk, Reed–Solomon
encodes the encrypted object, and assigns one opaque shard to each relay in a
keyed rendezvous-ranked constellation. A mature 12-relay constellation is
8-of-12: any eight shards reconstruct the object, while no relay stores the
whole network or even the whole object. The coding profile adapts to the
available network; today's two-relay network is necessarily 1-of-2 so either
relay can fail. Shard bytes live under `relay-data/<port>/shards` and only small
hash/size/deletion metadata lives in Turso, so media is never loaded into RAM
at startup. No Turso Cloud account, remote database, or auth token is involved.
A server deployment should use an explicit data directory on a durable volume:

```sh
noise-relay --listen 127.0.0.1:4301 --data /var/lib/noise-relay \
  --public-url https://relay.example
```

The same binary can put encrypted media in any S3-compatible object store
instead. S3 receives only the shards assigned to that relay, never a network
mirror. Configure its service environment and leave the command unchanged:

```sh
NOISE_STORAGE_BACKEND=s3
NOISE_S3_BUCKET=noise-relay-example
NOISE_S3_PREFIX=relay-1
NOISE_STORAGE_LIMIT_BYTES=1099511627776
AWS_DEFAULT_REGION=us-east-1
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
# Optional for R2, MinIO, Backblaze, or another compatible service:
AWS_ENDPOINT_URL_S3=https://s3.example
```

`NOISE_S3_PREFIX` is optional and defaults to `noise-relay`. Standard AWS
session-token, workload-identity, and container credentials are also supported.
HTTPS is required unless the operator explicitly sets `AWS_ALLOW_HTTP=true`.
`NOISE_STORAGE_LIMIT_BYTES` is optional; zero or unset means no application
quota beyond the disk/bucket's own limit. A bounded relay advertises its
remaining capacity in its signed v3 descriptor and rejects writes beyond the
allocation.
Secrets belong in a root-readable service environment file, not on the command
line. The included systemd units optionally load
`/etc/noise-relay/storage.env`; local-disk relays do not need that file at all.

The relay writes a shard to its configured destination before acknowledging it.
Shard IDs, provider locations, and deletion capabilities are carried inside the
encrypted media manifest. Relays cannot infer the group from a shard. Authorized
clients erase shards with per-object deletion capabilities; failed object-store
deletes stay in a durable retry queue.

This is a deliberately breaking storage cutover. Protocol v3 has no full-blob
upload/download endpoint and relays do not gossip media. On first v3 startup,
legacy full-blob rows and object files are purged rather than retained as dead
copies. Existing clients must update before the relays are upgraded; media
already cached on a device remains local, while uncached pre-v3 media is no
longer fetchable.

Then create two local identities:

```sh
RELAY_ONE=$(curl -fsS http://127.0.0.1:4301/v1/relay-descriptor)
RELAY_TWO=$(curl -fsS http://127.0.0.1:4302/v1/relay-descriptor)
cargo run -p noise-cli -- init --state .noise/alice.json --username alice --password 'violet-rivers-glow-after-midnight' --relay "$RELAY_ONE" --relay "$RELAY_TWO"
cargo run -p noise-cli -- init --state .noise/bob.json --username bob --password 'amber-clouds-drift-before-sunrise' --relay "$RELAY_ONE" --relay "$RELAY_TWO"
```

Create a group, copy the returned frequency, and join it from the second identity:

```sh
cargo run -p noise-cli -- make --state .noise/alice.json --name afterhours --relay "$RELAY_ONE" --relay "$RELAY_TWO"
cargo run -p noise-cli -- join --state .noise/bob.json --frequency "0000 0000 0000" --relay "$RELAY_ONE" --relay "$RELAY_TWO"
cargo run -p noise-cli -- say --state .noise/alice.json --text "hello" --relay "$RELAY_ONE" --relay "$RELAY_TWO"
cargo run -p noise-cli -- read --state .noise/bob.json --relay "$RELAY_ONE" --relay "$RELAY_TWO"
cargo run -p noise-cli -- members --state .noise/bob.json --relay "$RELAY_ONE" --relay "$RELAY_TWO"
```

## Membership scale simulation

Generate 50,000 identities and signed encrypted join events, then verify,
decrypt, and reduce them into one deterministic group view:

```sh
cargo run --release -p noise-sim -- --members 50000
```

This measures the membership log and client-side reducer. It does not pretend
to simulate 50,000 simultaneous sockets or a production group-key rotation.
The first recorded result is in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).


The twelve-digit frequency is an intentionally human-sized development
rendezvous code. It is not yet a production-grade capability secret; see
[`docs/PROTOCOL.md`](docs/PROTOCOL.md).
