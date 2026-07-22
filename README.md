# noise

Noise is a group-first messaging protocol. Groups are entered through numeric
frequencies and replicated across replaceable relays. Profiles require no phone
number, email address, or other personally identifying information.

The primary client is one React interface shared by Tauri on macOS and Windows
and, as the browser adapter comes online, the web. Tauri calls the shared Rust
client directly; the earlier native macOS client remains as a design reference.

## Workspace

- `noise-core`: identity, frequency, invitation, and signed-event primitives
- `noise-client`: reusable profile, group, and conversation operations
- `noise-ffi`: narrow native bridge into the shared client
- `noise-relay`: an untrusted store-and-forward relay with peer replication
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

### Install on another Mac

The second Mac needs Node.js, pnpm, Rust, and the Xcode command-line tools. Then:

```sh
git clone https://github.com/GnosysLabs/noise.git
cd noise/apps/client
pnpm install
pnpm tauri build --debug
open ../../target/debug/bundle/macos/noise.app
```

Clients must point at at least one relay they can both reach. Set a comma-separated
relay list at build time when the relays are not on the same machine:

```sh
VITE_NOISE_RELAYS=http://RELAY_HOST:4301 pnpm tauri build --debug
```

Localhost remains the default for development. The relay is currently in-memory,
so do not restart the existing development relays expecting their data to survive.

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

Start three relays in separate terminals:

```sh
cargo run -p noise-relay -- --listen 127.0.0.1:4301 --peer http://127.0.0.1:4302 --peer http://127.0.0.1:4303
cargo run -p noise-relay -- --listen 127.0.0.1:4302 --peer http://127.0.0.1:4301 --peer http://127.0.0.1:4303
cargo run -p noise-relay -- --listen 127.0.0.1:4303 --peer http://127.0.0.1:4301 --peer http://127.0.0.1:4302
```

Then create two local identities:

```sh
cargo run -p noise-cli -- init --state .noise/alice.json --username alice
cargo run -p noise-cli -- init --state .noise/bob.json --username bob
```

Make noise, copy the returned frequency, and join it from the second identity:

```sh
cargo run -p noise-cli -- make --state .noise/alice.json --name afterhours --relay http://127.0.0.1:4301
cargo run -p noise-cli -- join --state .noise/bob.json --frequency "0000 0000 0000" --relay http://127.0.0.1:4303
cargo run -p noise-cli -- say --state .noise/alice.json --text "hello" --relay http://127.0.0.1:4301
cargo run -p noise-cli -- read --state .noise/bob.json --relay http://127.0.0.1:4303
cargo run -p noise-cli -- members --state .noise/bob.json --relay http://127.0.0.1:4303
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
