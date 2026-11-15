# Handoff

Where Veil stands, for the next session. Read `docs/ARCHITECTURE.md` first.

## Done (all committed on `main`, local only, nothing pushed, no GitHub repo yet)

- `veil-security`: device keys, safety numbers, signed invites, per-frame encryption.
- `veil-topology`: mesh or forwarded layout, forwarder choice by policy, cascades, degradation.
- `veil-net`: QUIC links pinned to device keys (TLS 1.3 raw public keys), framed control
  stream, one stream per video frame, audio datagrams, parallel dialling, virtual sockets.
- `veil-rendezvous`: address reflection, introductions for hole punching, relay sessions that
  carry the members' own end-to-end QUIC link.

Tests: `cargo test -j 8 --workspace` (58 tests), `cargo clippy -j 8 --workspace --all-targets`.
Every test runs on localhost. Hole punching across real home routers is not yet tested.

## Open shortcuts (each marked `shortcut:` in the code)

- Relay has no per-session bandwidth cap or fair queuing. Needed before relaying for strangers.
- When both sides of a punch connect, the caller holds two links. Keep the lower key's dial.
- Video frames are read one at a time on the receiver; stale frames are not reset.
- Congestion control is quinn's loss-based default; real-time video needs a delay-based one.
- Every forwarder in a cascade receives every stream (topology planner).

## Next, in suggested order

1. `veil-group`: MLS (`openmls`) group state, lobby admission, epoch secrets into
   `veil-security::frame`. Check `openmls` licence first.
2. `veil-node`: one binary with relay, forwarder and rendezvous roles, configured by file.
3. Two-machine test of hole punching and relay over the real internet.
4. `veil-media`: capture and hardware encode per OS.
5. `veil-app`: `egui` and `wgpu` interface.
