# Handoff

Where Veil stands, for the next session. Read `docs/ARCHITECTURE.md` first.

## Done (all committed on `main`, local only, nothing pushed, no GitHub repo yet)

- `veil-security`: device keys, safety numbers, signed invites, per-frame encryption.
- `veil-topology`: mesh or forwarded layout, forwarder choice by policy, cascades, degradation.
- `veil-net`: QUIC links pinned to device keys (TLS 1.3 raw public keys), framed control
  stream, one stream per video frame, audio datagrams, parallel dialling, virtual sockets.
  `Control` now also carries `KeyPackage`, `Welcome` and `Commit` for MLS.
- `veil-rendezvous`: address reflection, introductions for hole punching, relay sessions that
  carry the members' own end-to-end QUIC link.
- `veil-group`: MLS group state (`openmls` 0.9), the host's lobby (one-time token, approve or
  decline, key package must be signed by the link's device key), a new epoch on every join or
  leave, and epoch secrets into `veil-security` frames with leaf index as sender index.

Tests: `cargo test -j 8 --workspace` (71 tests), `cargo clippy -j 8 --workspace --all-targets`.
Every test runs on localhost. Hole punching across real home routers is not yet tested.
`veil-group` needs Rust 1.91 (openmls); the other crates still build on 1.85.

## Licence note

`openmls_rust_crypto` and `openmls_basic_credential` are not used. The first pulls `hpke-rs`
(MPL-2.0). `veil-group/src/provider.rs` is Veil's own backend on RustCrypto plus `hpke` 0.13
(MIT or Apache-2.0), with one ciphersuite. Re-check licences when bumping openmls.

## Open shortcuts (each marked `shortcut:` in the code)

- Relay has no per-session bandwidth cap or fair queuing. Needed before relaying for strangers.
- When both sides of a punch connect, the caller holds two links. Keep the lower key's dial.
- Video frames are read one at a time on the receiver; stale frames are not reset.
- Congestion control is quinn's loss-based default; real-time video needs a delay-based one.
- Every forwarder in a cascade receives every stream (topology planner).
- The host is the only MLS committer, so the call ends when the host leaves (`group.rs`).
- MLS keys are kept in memory only; a restarted app rejoins as a new member (`provider.rs`).
- A joiner who knocks and disconnects keeps its lobby seat until the lobby is dropped
  (`lobby.rs`).

## Not yet tested

- A commit forged by a non-host member (the `NotFromHost` check) has no test, because openmls
  gives no easy way to build one without a second committer. Add one with `veil-node`.

## Next, in suggested order

1. `veil-node`: one binary with relay, forwarder and rendezvous roles, configured by file.
   Wire the lobby to real links: `Hello` and `KeyPackage` in, `Admitted`, `Welcome` and
   `Commit` out, and remove a member when its link closes.
2. Two-machine test of hole punching and relay over the real internet.
3. `veil-media`: capture and hardware encode per OS.
4. `veil-app`: `egui` and `wgpu` interface.
