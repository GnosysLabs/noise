# Protocol laboratory benchmarks

These results describe specific protocol operations on a specific machine.
They are not claims about concurrent sockets, relay throughput, production
encryption epochs, or end-user latency.

## 2026-07-22: 50,000-member reconstruction

Environment:

- Apple M5
- 16 GiB memory
- macOS 27.0
- rustc 1.96.1
- optimized `--release` build

Command:

```sh
cargo run --release -p noise-sim -- --members 50000 --relay-copies 3
```

Result:

```text
members reconstructed       50000
signed join events          50000
rejected events             0
membership log              26.04 MiB
membership across 3 relays  78.11 MiB
average join event          546 bytes
join generation             0.947s (52817 events/s)
verify + decrypt + reduce    1.118s (44740 events/s)
one encrypted message       546 bytes
stored once on 3 relays     1.60 KiB
naive per-member fanout      26.04 MiB
```

The run generated 50,000 distinct Ed25519 identities and 50,000 real encrypted,
signed membership events. The reducer verified every signature, decrypted every
payload, applied membership state, and accepted a message authored after the
first member joined.

This establishes that the current append-only membership representation is
locally tractable at the target member count. It does **not** validate member
removal key rotation, forward secrecy, 50,000 live connections, hostile relay
behavior, or network-wide propagation under load.
