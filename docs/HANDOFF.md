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
- `veil-forward`: the forwarder role as a library. Members join by room tag; sealed frames
  and audio datagrams go unchanged to every other member; per-receiver queues bounded in bytes.
- `veil-node`: one binary, `veil-node [CONFIG]`. TOML turns on rendezvous (relay optional) and
  forwarder, each on its own port, one device key from `identity_file` (created on first
  start). Prints `key@address` per role. Example: `crates/veil-node/veil-node.example.toml`.
- `veil-net::NodeAddr`: `key@ip:port`, so an invite's relay field can pin the relay's key.
- `veil-call`: participant sessions over real links. `HostHandle::start` / `serve` / `invite` /
  `decide` / `remove` and `join(node, invite, identity)`. Events carry `EpochKeys` (frame sender,
  receiver, roster) for every epoch. A closed link is a leave. `Lobby::forget` frees the seat of
  a joiner that disconnects in the lobby.
- Media links (`veil-call`): `CallNode` (`node.rs`) routes incoming links by first message
  (`inbound.rs`: `Hello` to the host, new `Control::Media` to media) and runs a rendezvous
  presence (`presence.rs`). `dial` tries direct, then punched (introduction), then relay.
  `join(&CallNode, &Invite)` uses it, so the invite's relay (`key@ip:port`) is the fallback.
  `routes.rs` turns a `veil-topology` plan plus a `Directory` (planner id to device key or
  forwarder `NodeAddr`) into `Routes::Mesh` or `Routes::Forwarder`. `MediaSession` (`media.rs`)
  applies routes, keeps one link per pair (lower key dials; a punch dial-back is closed), holds
  early inbound links up to 10 s until the routes expect them, and sends and receives sealed
  frames and audio. `KeyRing` (`keyring.rs`) opens the previous epoch for 2 s after a change.
  `CallNode::set_relay_only` hides the participant's address from other members.
- Plan distribution (`veil-call/src/call_plan.rs`): the host plans each epoch and calls
  `HostHandle::publish_plan(plan, directory)`. The actor refuses a directory that does not list
  exactly this epoch's members, resolves it to a `CallPlan` (layout plus every member's device
  and addresses), signs it (domain tag, room id, epoch) and queues `Control::Plan` behind the
  epoch's commit. Members check it against the invite's host key, room and their own epoch and
  get `MemberEvent::Routes`, or `MemberEvent::PlanRefused` (old routes stand). The app applies
  the routes to its `MediaSession`.
- Roster tickets (`veil-forward/src/ticket.rs`): a forwarded `CallPlan` carries a host-signed
  `RosterTicket` (room tag, host key, epoch, every member's device key). `Join` must carry one
  and the forwarder admits only listed devices. Forwarder rooms are keyed by (room tag, host
  key), so a stranger's self-signed ticket only opens a room of its own. Epochs only move
  forward: an older ticket is refused (`StaleTicket`), and a newer one (on a join, or
  `ForwarderClient::update_ticket`, which `MediaSession` sends on every re-plan) drops members
  it no longer lists. The local `routes()` helper is gone: routes come from the host's plan.
- `veil-probe`: a call with no camera, for the two-machine test. `veil-probe host` prints an
  invite and admits whoever uses it; `veil-probe join LINK` joins. Both send a sealed probe
  every second as a video frame and as an audio datagram, and every 10 s print per member and
  channel the reply rate, median and worst round trip. Each link's path (direct, punched,
  relayed, forwarder) is printed when routes apply. Run steps below.
- Layer markers (`veil-security/src/layer.rs`): the sealed frame header is now 18 bytes, key id,
  counter, then a two-byte `Layer` (spatial 0 to 2, temporal 0 to 3, keyframe flag), all in the
  AEAD's associated data. `FrameSender::encrypt_layer` sets it (`encrypt` seals `Layer::BASE`),
  `frame::frame_layer` reads it without opening. Unknown flag bits are refused.
- Layer subscriptions (`veil-forward/src/subscribe.rs`): `ClientMsg::Subscribe` names the highest
  `Wanted` layers per sender. Lowering is immediate, raising waits for that sender's next
  keyframe, unnamed senders get every layer, at most 256 senders tracked per receiver. A frame
  whose marker does not parse is dropped. `ForwarderClient::subscribe` and
  `MediaSession::subscribe` send it.

Tests: `cargo test -j 8 --workspace` (139 tests), `cargo clippy -j 8 --workspace --all-targets`.
Every test runs on localhost. Hole punching across real home routers is not yet tested.
`veil-group` needs Rust 1.91 (openmls); the other crates still build on 1.85.

## Licence note

`openmls_rust_crypto` and `openmls_basic_credential` are not used. The first pulls `hpke-rs`
(MPL-2.0). `veil-group/src/provider.rs` is Veil's own backend on RustCrypto plus `hpke` 0.13
(MIT or Apache-2.0), with one ciphersuite. Re-check licences when bumping openmls.

## Open shortcuts (each marked `shortcut:` in the code)

- Relay has no per-session bandwidth cap or fair queuing. Needed before relaying for strangers.
- Video frames are read one at a time on the receiver; stale frames are not reset.
- Congestion control is quinn's loss-based default; real-time video needs a delay-based one.
- Every forwarder in a cascade receives every stream (topology planner).
- The host is the only MLS committer, so the call ends when the host leaves (`group.rs`).
- MLS keys are kept in memory only; a restarted app rejoins as a new member (`provider.rs`).
- A receiver that raises its layers waits for the sender's periodic keyframe; there is no
  keyframe request yet (`veil-forward/src/subscribe.rs`).
- In a mesh every layer still arrives; subscriptions work only through a forwarder, and a new
  forwarder link starts with every layer, so the app subscribes again after routes change
  (`veil-call/src/media.rs`).
- A forwarder room's tickets have no expiry. A removed member holding an old ticket can open
  an empty room after everyone leaves; it receives nothing, and is dropped as soon as a real
  member joins with a newer ticket. Add an expiry if empty-room squatting matters.
- No forwarder-to-forwarder cascade yet: one forwarder per room (`veil-forward/src/server.rs`).
- `NodeAddr` takes a literal IP only, no DNS names (`veil-net/src/node_addr.rs`).
- A plan carries at most 128 members so it fits one control message (`call_plan.rs`).
- The app must call `publish_plan` after each `HostEvent::Keys`; the session does not plan by
  itself. A member sees no routes for an epoch until the host publishes.
- A plan with more than one forwarder is refused until `veil-forward` cascades
  (`veil-call/src/call_plan.rs`).
- Forwarder links are direct only, no relay fallback (`veil-call/src/media.rs`).
- Host events are an unbounded channel (`veil-call/src/host.rs`).

## Not yet tested

- The automatic step from a failed punch to the relay. On localhost a punch always succeeds,
  so the relay path is tested through relay-only mode, and the failed direct dial falls back
  to the punch. Covered properly by the two-machine test.

- A commit forged by a non-host member (the `NotFromHost` check) has no test, because openmls
  gives no easy way to build one without a second committer. In `veil-call` only the host
  sends commits, so this needs a deliberately broken host to test.

## Running the two-machine test

1. Public box: `veil-node` with rendezvous, relay and forwarder on (see the example TOML).
   It prints `rendezvous KEY@IP:PORT` and `forwarder KEY@IP:PORT`. It prints the listen address
   (`0.0.0.0`), so replace that with the box's public IP. Open 4433/udp and 4434/udp.
2. Home network A: `veil-probe host --rendezvous KEY@IP:PORT`. Add `--advertise PUBLIC_IP:7710`
   if port 7710/udp is forwarded to this machine. It prints an invite link.
3. Home network B: `veil-probe join LINK`. Check the safety numbers match on both sides.
4. Read the `link to ...` lines: `Punched` means hole punching worked. Run again with
   `join LINK --relay-only` to test the relay, and with `host --forwarder KEY@IP:PORT` to test
   forwarding. Each invite is one use; the host prints a fresh one after every join.

## Next, in suggested order

1. Two-machine test over the real internet with `veil-probe` (steps above). Needs Jeremy's
   machines and a public box.
2. `veil-media`: capture and hardware encode per OS, feeding `MediaSession` and `KeyRing`.
   The layer marker and forwarder side are done; the encoder sets `Layer` per frame. Decide
   first: OS capture and Media Foundation need `unsafe` FFI, which the workspace forbids, so
   `veil-media` needs its own lint exception; and Opus means libopus (BSD, C build).
3. `veil-app`: `egui` and `wgpu` interface.
