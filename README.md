# Veil

Peer-to-peer video calls in Rust. End-to-end encrypted, built for 4K at 60 fps and low latency,
with no reliance on cloud compute. Servers are optional: any participant, a box you run, or an
enterprise cluster can carry a call, and none of them can read it.

Motto: modular, agnostic, shared.

Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

Veil is early and unaudited. [docs/TRANSPARENCY.md](docs/TRANSPARENCY.md) says what it depends on,
what other parties can see, where it uses code Rust cannot check, and what is not finished.

## Download

Windows builds are on the [latest release](https://github.com/Jeremy8776/veil/releases/latest):
a zip with `veil-app.exe`, `veil-node.exe` and `veil-probe.exe`, and `SHA256SUMS.txt` to check
it. Unzip anywhere and run `veil-app.exe`; no install or admin rights are needed. The build is
not signed yet, so Windows SmartScreen says "unknown publisher": choose More info, then Run
anyway. The app tells you when a newer release is out; it never updates itself.

## Build and test

```bash
cargo test -j 8 -p veil-security -p veil-topology
```

Check dependency licences (fails on a licence that is not allowed, or a stale list):

```bash
python tools/licences.py --check
```

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT),
at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in
Veil, as defined in the Apache-2.0 licence, is dual licensed as above, without any additional
terms or conditions.
