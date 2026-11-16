# Handoff

Where Veil stands, for the next session. Read `docs/ARCHITECTURE.md` first.

## Done (all on `main`, pushed to the private repo Jeremy8776/veil)

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
  `MediaSession::subscribe` send it. `Wanted` now lives in `veil-security::layer` (re-exported
  by `veil-forward`) so `veil-media` can use it without the network stack.
- `veil-media` core, OS-independent, safe Rust:
  - `ladder.rs`: the bitrate ladder. `Ladder::camera()` is Thumb 640x360 at 15 fps, Hd
    1920x1080 at 30 fps, Uhd 3840x2160 at 60 fps, from 60 fps capture with 3 temporal levels.
  - `temporal.rs`: the dyadic temporal pattern (3 levels: 0, 2, 1, 2).
  - `schedule.rs`: `Schedule::next(now)` plans each captured picture: number, temporal layer,
    keyframe, reference picture, and the `Layer` markers to encode. Layers nobody wants are not
    encoded (`set_wanted`, `wanted_by`). Lowering is immediate; a raise or resume waits for a
    keyframe. Keyframes at most every 500 ms, at least every 4 s, and on `request_keyframe`.
  - `payload.rs`: `VideoHeader`, 27 bytes inside the sealed frame (codec, picture, reference
    distance, `needs_lower` for SVC, capture time, size), then the bitstream.
  - `gate.rs`: `Gate::admit(layer, header)` per sender. Passes a layer only if its reference
    was decoded; asks for a keyframe (`DropAndRequestKeyframe`) only when a temporal 0 picture
    cannot decode, once per break.
  - `encoder.rs`: `VideoEncoder` trait (NV12 `RawPicture` plus `PicturePlan` in, one
    `EncodedLayer` per planned layer out), and `check_output`.
- Windows H.264 encoder (`veil-media`), simulcast: `media_foundation(&ladder, hardware)` gives a
  `Simulcast<MfRung>`, one Media Foundation encoder per rung. It tries the GPU encoders from
  `MFTEnumEx` first (on this machine: NVIDIA H.264 Encoder MFT) and falls back to Microsoft's
  software encoder. Low latency, CBR, no B pictures, no encoder-chosen IDR, one access unit out
  per picture in; an encoder that holds pictures back is refused.
  - `simulcast.rs` (safe, OS-independent): feeds each rung every picture with
    `temporal <= top_temporal`, even when the plan lowered its temporal layers (the output is
    dropped), because each encoder runs its own temporal pattern
    (`CODECAPI_AVEncVideoTemporalLayerCount`, which must be set before the media types). A rung omitted from a temporal 0
    picture is paused and is reopened (`RungEncoder::restart`) at the next keyframe. Every
    output is checked: a planned keyframe must contain an IDR, and the prefix unit's temporal
    id must match the plan.
  - Found on real encoders (15/11/2026): both encoders honour a forced IDR only at the start of
    their temporal pattern, and an IDR does not restart the pattern. So `Schedule` now places
    keyframes only where `phase % period == 0`, up to 4 pictures (67 ms at 60 fps) late.
  - `scale.rs`: CPU box filter from the captured picture to each rung's size. `annexb.rs`: reads
    NAL unit types, `nal_ref_idc` and prefix temporal ids.
  - `ffi/windows/` is the only `unsafe` code in Veil (`mod.rs`, `enumerate.rs`, `sample.rs`).
    `veil-media` sets `unsafe_code = "deny"` and `clippy::undocumented_unsafe_blocks = "deny"`
    in its own lint table (it copies the workspace's clippy lints; keep them in step).
  - Test: `tests/media_foundation.rs` encodes 72 synthetic 1080p NV12 pictures through the
    Thumb and Hd rungs on hardware and software, with a requested keyframe and an Hd pause and
    resume. It checks IDRs, P slices, sizes and that Hd temporal 1 slices are non-reference.
    It skips with a note when no encoder exists. The Uhd rung (3 temporal levels) is covered
    only by the fake encoder and the prefix temporal id check, not by a real 4K run.
- Keyframe requests end to end. `MediaSession::request_keyframe(sender)` sends
  `Control::KeyframeRequest` on that member's media link in a mesh, or
  `ClientMsg::KeyframeRequest(sender)` to the forwarder, which passes `ServerMsg::KeyframeRequest`
  to that sender only, without saying who asked. The forwarder also asks a sender itself when a
  receiver's subscription raise waits for a keyframe. The sender sees `Media::KeyframeRequest`
  from `MediaSession::recv` and calls `Schedule::request_keyframe`, which limits keyframes to
  one per 500 ms. Requests are never queued behind video: a full queue drops them.
  `ForwarderClient::recv` reads the forwarder's messages after joining.
- Windows camera capture (`veil-media`). `cameras()` lists capture devices (name, id).
  `MfCamera::open(id, CameraMode)` picks a native mode with `capture::rank_modes` (frame rate
  first, then the smallest size that covers the wanted one, then the cheapest format: NV12,
  YUY2, MJPEG), and the source reader's advanced video processing converts it to NV12.
  `MfCamera::read()` blocks for the next picture and gives a tightly packed `RawPicture`
  (`capture::pack_nv12` copies it out of the locked buffer within the bounds Windows reports).
  `ffi/windows/camera.rs`. Test: `tests/camera.rs`; listing runs always (finds the OBSBOT Tiny
  2 Lite here), live capture is `#[ignore]` because it turns the camera on:
  `cargo test -j 8 -p veil-media --test camera -- --ignored --nocapture`. Passed 15/11/2026:
  OBSBOT opened at 1920x1080, 30 fps, native NV12, 30 pictures with rising timestamps.
- Opus audio (`veil-media`), every platform. libopus 1.6.1 sources are vendored in
  `crates/veil-media/vendor/opus` (`VENDORED.md`: origin, what was left out, how to update) and
  `build.rs` compiles them with `cc`, so no cmake is needed. Float build, no SIMD, no
  deep-learning features. `ffi/opus.rs` is the libopus FFI: `OpusEncoder` (VoIP, voice signal,
  VBR, in-band FEC, `set_bitrate`, `set_expected_loss`) and `OpusDecoder` (`decode`, `conceal`
  for a lost frame, `recover` a lost frame from the next packet's FEC). `audio.rs`:
  `AudioSettings::voice()` (mono, 20 ms, 32 kbit/s, FEC for 10% loss) and `AudioHeader`, 13 bytes
  inside the sealed datagram (version, sequence number, capture time), then the Opus packet.
  Test: `tests/opus.rs` (tone round trip, concealment and FEC recovery, bad input).
- Windows H.264 decoder (`veil-media`). `VideoDecoder` trait (`decoder.rs`): one access unit in,
  at most one packed NV12 `RawPicture` out, one decoder per sender and spatial layer. Call it
  only for layers `Gate` passes. `check_size` refuses a decoded size above 4K (3840x2176 coded)
  before allocating. `MfDecoder` (`ffi/windows/decoder.rs`) is Microsoft's decoder transform,
  low latency, so each access unit gives its picture at once; a picture still held is dropped
  before the next input. It crops the padded coded height (1088) to the display area (1080),
  and follows a change of rung at a keyframe without reopening. The NV12 copy out of a Media
  Foundation buffer (`sample.rs::copy_nv12`) is now shared with the camera.
  Test: `tests/mf_decoder.rs` decodes the software and hardware encoders' Thumb and Hd output
  (sizes, timestamps, Hd luma within 6 levels on average), a switch from Thumb to Hd at a
  keyframe, and bad input (no picture, then recovery at a keyframe).
- Audio jitter buffer (`veil-media/src/jitter.rs`, OS-independent). `JitterBuffer::push(header,
  packet, arrival_us)` and, once per frame on the playback clock, `pop()`: `Packet` to decode,
  `Recover(next)` for `OpusDecoder::recover` (FEC), `Conceal`, or `Wait` (silence while
  filling). Target delay is the decaying worst arrival jitter in frames plus one, 1 to 10. A
  missing packet is waited for (one concealed frame, the delay grows) until packets up to the
  target depth after it arrive; then it is lost. A backlog over twice the target plus two is
  skipped down to the target. 200 ms with nothing to play refills. Sequence numbers wrap; at
  most 64 packets held; a jump of 64 or more resets.
- Windows microphone and speakers (`ffi/windows/audio.rs`). `WasapiCapture::open(&settings)` and
  `read()` (one frame, interleaved float, waits on the device), `WasapiPlayback::open` and
  `write(frame)` (waits while two frames are queued, so it paces the caller; about 40 ms on the
  device) and `queued_ms()`. Default communications device, shared mode, Windows converts to
  48 kHz float; communications category, so Windows applies the device's echo cancellation
  where it has one. New `MediaError::Playback`.
  Tests: `jitter.rs` unit tests; `tests/opus.rs` sends a tone over a simulated network (10 to
  50 ms delay, every 7th packet lost) through the buffer and decoder: all 8 losses rebuilt
  from FEC, 3 frames concealed while the delay grows. `tests/wasapi.rs` plays 25 frames of
  silence (about 430 ms here) and, `--ignored`, reads 50 microphone frames:
  `cargo test -j 8 -p veil-media --test wasapi -- --ignored --nocapture`.

- `veil-app`, the desktop interface (`eframe` 0.37 with `wgpu`), demo only. Run it with
  `cargo run -j 8 -p veil-app --release`. Screens (`src/screens/`): home, pre-join, call (top
  bar, gallery or speaker stage, control bar), side panels (people and lobby, chat, security),
  and the end screen. `model.rs` is the call state the screens read; `demo.rs` fills it with
  four simulated people who take turns speaking, a knock in the lobby at 8 s, and a chat
  message at 14 s, behind a banner. `media.rs` runs your real camera (`MfCamera`, converted to
  RGBA at half size in `convert.rs`) and microphone (`WasapiCapture`, for the meter and your
  speaking ring) on threads; muting or stopping video closes the device. Ctrl+Shift+M mutes,
  Ctrl+Shift+O toggles video. Fonts and icons come from Windows (`theme.rs`, `icons.rs`), none
  bundled; icon codepoints were checked against both Segoe icon fonts by rendering them.
  Tests: 20 unit tests (grid layout, NV12 conversion, meter, model, demo timeline).
  Not yet looked at by a person: the app has not been run on screen by anyone but its tests.

Tests: `cargo test -j 8 --workspace` (233 tests, two ignored; `veil-media` alone: `cargo test -j 8 -p veil-media`), `cargo clippy -j 8 --workspace --all-targets`.
Every test runs on localhost. Hole punching across real home routers is not yet tested.
`veil-group` needs Rust 1.91 (openmls); the other crates still build on 1.85.

## Licence note

Run `python tools/licences.py --check` after any `Cargo.lock` change; it regenerates nothing
and fails on a disallowed licence or a stale `docs/DEPENDENCIES.md` (rewrite it without
`--check`). Veil is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, set once in the
workspace `Cargo.toml`; each crate has `license.workspace = true`).

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
- The forwarder sends every spatial layer up to the wanted one, which wastes download for
  simulcast (`subscribe.rs`).
- A layer that arrives before its reference is dropped, not reordered (`veil-media/src/gate.rs`).
- The 4 s periodic keyframe is the fallback for lost requests (`veil-media/src/schedule.rs`).
- Scaling to each rung is a CPU box filter; move it to the GPU when capture gives textures
  (`veil-media/src/scale.rs`).
- A paused rung's encoder is reopened on resume, a stall of tens of milliseconds
  (`veil-media/src/ffi/windows/mod.rs`).
- The async (hardware) encoder is polled every 0.2 ms for events (`ffi/windows/mod.rs`).
- Each camera picture is copied once on the CPU (`ffi/windows/camera.rs`). A camera slower
  than the ladder's capture rate (say 4K at 30 only) is used at 1080p60 if it offers that;
  the app must still fit the ladder to the mode it gets (`Ladder::up_to`).
- libopus is built without SSE, AVX or NEON (`veil-media/build.rs`).
- `veil-app` converts camera pictures to RGBA on the CPU (`convert.rs`), shows only your own
  video, and simulates everyone else (`demo.rs`). Screen sharing says it is not built yet.
- The jitter buffer catches up by skipping packets, which clicks; time-stretch instead if it
  is audible (`veil-media/src/jitter.rs`).
- WASAPI uses the default communications device only; a lost device is an error and the app
  opens again (`veil-media/src/ffi/windows/audio.rs`). No echo cancellation of Veil's own
  where the device has none.
- H.264 decoding is on the CPU; the GPU path needs a D3D11 device manager
  (`veil-media/src/ffi/windows/decoder.rs`).
- Opus runs at a variable bitrate, which can leak words through packet sizes; no constant
  bitrate setting yet (`veil-media/src/ffi/opus.rs`).
- The vendored libopus has not been compared against the xiph.org release tarball
  (`vendor/opus/VENDORED.md`).

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
2. `veil-media`, continued: the core, keyframe requests, the Windows H.264 encoder and
   Windows camera capture, Opus, the H.264 decoder, the jitter buffer and WASAPI are done
   (see above). Windows media is complete for a first call. Not yet run: the microphone test
   (`--ignored`). macOS and Linux back ends come later. Decided
   15/11/2026: `unsafe` FFI and libopus are allowed in `veil-media` only, under the rules in
   `docs/TRANSPARENCY.md` section 5 (one `ffi` module per OS, `// SAFETY:` on every block,
   `clippy::undocumented_unsafe_blocks` denied, each module listed in that table).
3. `veil-app`: the interface is built as a demo (see above). Next: Jeremy runs it and reports
   what to change; then wire it to `veil-call` (start or join, lobby events, plan publishing)
   and `veil-media` (encode, seal, send; receive, gate, decode, play), replacing `demo.rs`.
