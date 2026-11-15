# Veil

Peer-to-peer video calls in Rust. End-to-end encrypted, built for 4K at 60 fps and low latency,
with no reliance on cloud compute. Servers are optional: any participant, a box you run, or an
enterprise cluster can carry a call, and none of them can read it.

Motto: modular, agnostic, shared.

Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build and test

```bash
cargo test -j 8 -p veil-security -p veil-topology
```
