# Handoff

Where Veil stands, for the next session. Read `docs/ARCHITECTURE.md` first.

## Done (all on `main`, Jeremy8776/veil)

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
  frames and audio. `KeyRing` (`keyring.rs`) opens the previous epoch for 2 s after a change,
  or not at all when the change removed a device.
  `CallNode::set_relay_only` hides the participant's address from other members.
- Plan distribution (`veil-call/src/call_plan.rs`): the host plans each epoch and calls
  `HostHandle::publish_plan(plan, directory)`. The actor refuses a directory that does not list
  exactly this epoch's members, resolves it to a `CallPlan` (layout plus every member's device
  and addresses), signs it (domain tag, room id, epoch) and queues `Control::Plan` behind the
  epoch's commit. Members check it against the invite's host key, room and their own epoch and
  get `MemberEvent::Routes`, or `MemberEvent::PlanRefused` (old routes stand). The app applies
  the routes to its `MediaSession`.
- App messages (`veil-call/src/app_msg.rs`, 16/11/2026): `AppMessage::Name` and `Chat`,
  plaintext `veil-app` plus version 2, an Ed25519 signature by the sender's device key over
  the frame id (key id and counter) and the body, then the postcard body, sealed with the
  epoch's `FrameSender` (`AppMessage::seal`). The receiver opens it with `AppMessage::open`
  and the roster's device key for the frame's leaf, so the sender is the signing device, not
  just a leaf index any member could claim, and replays hit the frame replay window.
  Checked both ways: names 1 to 64 characters, chat 1 to 2,000, no
  control characters (chat may hold line breaks and tabs), no bidirectional overrides; a
  sealed message is at most `veil_net::control::MAX_APP_LEN` (16 KiB). In a mesh it goes as
  `Control::App` on each media link's control stream (`MediaSession::send_app`, received as
  `Media::App`); through a forwarder as `ClientMsg::App`, which the forwarder passes unread
  to every other member as `ServerMsg::App` (dropped if a member's control queue is full).
  Tested in `tests/media.rs`, mesh and forwarded.
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
  constant bitrate by default (`AudioSettings::constant_bitrate`), in-band FEC, `set_bitrate`, `set_expected_loss`) and `OpusDecoder` (`decode`, `conceal`
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

- `veil-app`, the desktop interface (`eframe` 0.37 with `wgpu`). Run it with
  `cargo run -j 8 -p veil-app --release`. Screens (`src/screens/`): home, pre-join, call (top
  bar, gallery or speaker stage, control bar), side panels (people and lobby, chat, security),
  and the end screen. `model.rs` is the call state the screens read; `demo.rs` fills it with
  four simulated people who take turns speaking, a knock in the lobby at 8 s, and a chat
  message at 14 s, behind a banner, only from the "Demo call" entry on Home. `media.rs` runs your real camera (`MfCamera`, converted to
  RGBA at half size in `convert.rs`) and microphone (`WasapiCapture`, for the meter and your
  speaking ring) on threads; muting or stopping video closes the device. Ctrl+Shift+M mutes,
  Ctrl+Shift+O toggles video. Fonts and icons come from Windows (`theme.rs`, `icons.rs`), none
  bundled; icon codepoints were checked against both Segoe icon fonts by rendering them.
  Device choice (asked 16/11/2026): `veil_media::microphones()` and `speakers()` list active
  WASAPI devices (`audio_devices.rs`), `WasapiCapture::open` and `WasapiPlayback::open` take
  a device id or `None` for the default. In the app, `devices.rs` reads cameras, microphones
  and speakers when pre-join opens, and pre-join has a drop-down for each; changing camera or
  microphone reopens it. The call's playback opens the chosen speaker.
  `tests/wasapi.rs` lists devices and opens a speaker by id.
- `veil-app` real calls (16/11/2026), `src/engine/`. `Engine::start(Role::Host | Guest)`
  owns a Tokio runtime (2 workers) and three threads; dropping it leaves the call (it waits up
  to about a second for links to close). The screens still read only `model.rs`:
  `Engine::sync` copies the engine's view (status, invite, epoch, people, lobby) into `Call`
  each frame, and the host's Admit, Decline and Remove go out through `Call::outbox` to
  `HostHandle::decide` and `remove`. `Call::live` starts a real call; `CallStatus` is
  Connecting, Lobby (guest waiting), Live or Ended with a reason the end screen shows.
  - `host.rs`: binds `0.0.0.0` (port from `VEIL_PORT` or any), runs its own
    `RendezvousServer` (rendezvous plus relay) on a second port unless `VEIL_RENDEZVOUS` names
    a `veil-node`, and puts in the invite the `VEIL_ADVERTISE` addresses, the LAN address
    (found by asking the routing table, nothing sent) and 127.0.0.1, so a second copy on the
    same computer can join. A fresh invite after every knock (tokens are one use). After
    every `HostEvent::Keys` it plans a mesh (`plan.rs`, as `veil-probe`) and calls
    `publish_plan`.
  - `guest.rs`: `CallNode::for_invite` and `join`; the lobby wait can be cancelled by Leave.
    `MemberEvent::Routes` is applied to the `MediaSession`; Removed or Closed end the call.
  - `session.rs`: seals each encoded layer with `encrypt_layer` and each Opus frame with
    `encrypt`, sends through the `MediaSession`, opens incoming media through a `KeyRing` and
    maps the authenticated leaf index to a participant. Nothing is encoded or sent while you
    are alone. Link paths refresh once a second for the Security panel.
  - `video_out.rs`: `ladder_for` fits the camera (720p30 gives Thumb 640x360 at 15 fps and
    Hd 1280x720 at 30 fps; a camera of 640 or less sends one layer; capped at 1080p), then
    `Schedule` and `media_foundation(&ladder, true)`. A failed encoder reopens after 5 s.
  - `video_in.rs`: per sender, follows the largest spatial layer that keeps arriving (drops
    to the next after 1 s without it), `Gate`, `MfDecoder`, NV12 to RGBA (half size above 960
    wide) for that person's tile. A broken stream asks for a keyframe.
  - `audio_out.rs`: per sender a `JitterBuffer` and `OpusDecoder` (decode, FEC recover or
    conceal), mixed, clamped and written to `WasapiPlayback`, which paces the loop. Each
    person's level drives their speaking ring; no packet for a second shows them muted, no
    picture for two seconds shows their camera off.
  - `media.rs` hands camera pictures and microphone frames to the engine through `Taps`
    (bounded; a full queue drops).
  - Names and chat (16/11/2026): `Engine::start` takes your name. `names.rs` keeps the names
    others sent, by device (until then "Host", or "Guest" and a key prefix; the lobby always
    shows the key prefix, since a knocker is not a member yet), and sends yours once a second
    for 5 s after each key epoch, new routes or new link, so a member that changes epoch a
    moment later still gets it. `Call::send` shows your message and queues `Action::Chat`;
    the session seals it and sends it; received chat goes through `Shared::take_chat` into
    `Call::receive` (counted unread). The name field takes 64 characters, chat 2,000.
    Raised hands are still local only.
  - Forwarder (16/11/2026): up to 4 people the call is a mesh; from 5 the host plans every
    member through a forwarder (`plan.rs`, `Policy::mesh_max`). The forwarder is
    `VEIL_FORWARDER=KEY@IP:PORT` (a `veil-node`) if set; otherwise, when the host runs its own
    rendezvous, it also runs its own `ForwarderServer` on a third port at its LAN address
    (`host.rs::own_server`). With `VEIL_RENDEZVOUS` and no `VEIL_FORWARDER` there is none and
    a large call stays a mesh. Fixed on the way: a 5th person used to make the plan
    infeasible, and the error ended the host's call; now no forwarder means a mesh of any
    size, and a failed plan only leaves the old routes. Not yet run with 5 real copies.
  - Layer subscriptions (16/11/2026, `wanted.rs`): the call screen reports each other
    person's tile width in physical pixels (`Engine::watch`). A tile up to 720 px wide asks
    for the lowest spatial layer at every frame rate, a wider one (the speaker stage, a big
    gallery tile) for everything, and someone off screen for the lowest. The session sends
    the request with `MediaSession::subscribe` on its 1 s tick when it changes, and again
    after every new route. In a mesh it does nothing.
  Tests: 46 unit tests, plus `engine/loopback.rs` (ignored: it opens ports, the encoder and the
  speaker): two engines on localhost, the guest knocks, the host admits, both show the same
  safety number and epoch, synthetic 720p pictures and silence go both ways until each side
  decodes the other's video and hears packets, each shows the other's chosen name, a chat
  message goes each way, then the guest leaves. Passed 16/11/2026 in 1.9 s (host link
  Punched, guest link Direct):
  `cargo test -j 8 -p veil-app loopback -- --ignored --nocapture`. The same filter runs
  `loopback_leaving_a_call_with_devices_on_is_quick` (camera, microphone and a hosted call,
  then dropped in the app's order): 123 ms for the call, 113 ms for the devices (16/11/2026).
  Not yet run by a person: two copies of the app calling each other with real devices. Use
  headphones: there is no echo cancellation of Veil's own.
- Second audit, H1, fixed 16/11/2026: every member holds the epoch secret, so any member
  could seal a name or chat message under another member's leaf and it showed as theirs.
  App messages are now signed by the sender's device key, bound to the frame id
  (`app_msg.rs`, tests `a_member_cannot_speak_as_another` and
  `a_signed_message_cannot_move_to_another_frame`). Video and sound are still only
  group-authenticated (TRANSPARENCY.md, known leaks). Open from that audit: L2 to L4.
  L1 (CI actions on floating tags) fixed 16/11/2026: pinned to commit SHAs.
- Second audit, M1, fixed 16/11/2026: `KeyRing` kept the previous epoch open for 2 s after
  every change, so a member removed by that change could still inject video and sound under
  the old key. Every member can derive every sender's key from the epoch secret, so the old
  epoch cannot be limited to the leaves that stay. `KeyRing::new` and `rotate` now take the
  epoch's roster; if a device in the old roster is missing from the new one, the old receiver
  is dropped at once. Joins keep the grace. Tests: `keyring.rs`
  (`a_removal_closes_the_previous_epoch_at_once`) and `tests/media.rs`
  (`a_removed_member_gets_no_grace_on_the_old_epoch`).
- Audit low findings, fixed 16/11/2026:
  - L3, rendezvous: a request for a key that is not in the room gets no answer, the same as
    a member that declines, and request limits count it the same way. `PeerNotInRoom` is now
    sent only when a member asks for itself.
  - L1, early media links (`veil-call/src/media/links.rs`): only a lower key may wait. Members
    of the newest epoch share 16 places; any other key one of 4. `MediaSession::members(roster)`
    must be called on every new epoch, before applying its routes (the app, `veil-probe` and
    `tests/media.rs` do). So an old invite holder cannot crowd out members.
  - L2, video frames: `veil_net::FrameReader` reads up to 16 frame streams at once, drops a
    frame not complete within 2 s, and grows memory only as bytes arrive. Media sessions and
    the forwarder use it; `Link::recv_frame` stays for tests (`tests/frames.rs`).
  - L7, CI: `.github/workflows/ci.yml` runs clippy and tests on Windows, `cargo audit` and
    the licence check on Linux. `.cargo/audit.toml` lists the advisories accepted and why.
  - M4, saved device key (16/11/2026, `veil-app/src/keystore.rs`): one device key for every
    call in `%LOCALAPPDATA%\Veil\device.key`, and the keys you mark as verified in
    `verified.keys`, both sealed with DPAPI (`veil_media::protect`, `ffi/windows/protect.rs`).
    A file that does not open is moved to `*.unreadable` (`.2` and on, never over an earlier
    one) and a fresh one started. Reviewed 16/11/2026: a file that could not be read (locked,
    no permission) used to be written over by the fresh key; now it is left in place and that
    run's key or list stays in memory. Writes are flushed before the rename, and DPAPI's
    output buffer and the opened key bytes are wiped. The lobby
    shows "Verified" or "New device"; Security has Mark verified and Forget
    (`Action::Verify`, saved by `Engine::act`); Home has "New device key" (two clicks).
    Verified never admits anyone. Off Windows nothing is saved yet (`shortcut:`).
  - L4, names (`veil-app/src/engine/names.rs::labels`): a name that anyone else in the call
    shows, you included, gets that person's key prefix in people, lobby and chat. Names match
    ignoring case, spacing and a copied "(prefix)". Look-alike letters are a `shortcut:`.
  - L6, vendored libopus: every file matches xiph's `opus-1.6.1.tar.gz` (checksum and
    method in `crates/veil-media/vendor/opus/VENDORED.md`).
  - L5, quick-xml advisories (RUSTSEC-2026-0194, -0195): the old quick-xml came from
    `zbus_xml` 5.1 (Linux accessibility, under eframe), not the Wayland scanner. `zbus_xml`
    5.2.1 parses without quick-xml. The resolver held it back only because the workspace says
    Rust 1.85 and it asks for 1.87; it is Linux-only, so the Windows build is unchanged.
    `.cargo/audit.toml` accepts nothing now. Do not run a plain `cargo update` expecting it to
    stay: re-check `cargo tree -i quick-xml` after any lockfile change.
  Every audit finding (M1 to M4, L1 to L7) is now fixed.

Tests: `cargo test -j 8 --workspace` (233 tests, two ignored; `veil-media` alone: `cargo test -j 8 -p veil-media`), `cargo clippy -j 8 --workspace --all-targets`.
Every test runs on localhost. Hole punching across real home routers is not yet tested.
`veil-group` needs Rust 1.91 (openmls); the other crates build on 1.88 (`sha2` 0.11.1 and
`ctutils` 0.4.4 or later need it).

## Releases (from 16/11/2026)

`.github/workflows/release.yml`. A tag `vX.Y.Z` builds `veil-app`, `veil-node` and
`veil-probe` with Rust 1.98.1 and no cache, and packages them (from 17/11/2026, the same
approach as ArtCraft's packaging):

| Platform | Files | Script |
|---|---|---|
| Windows x64 (`windows-2025`, static CRT) | `.msi` for all users, with the "Veil" UDP firewall rule; `-portable.zip` | `packaging/windows/package.ps1`, `veil.wxs`, `sign.ps1` |
| macOS universal (`macos-15`) | `.dmg` with `Veil.app` (hardened runtime, camera and microphone entitlements); `.tar.gz` with `veil-node` and `veil-probe` | `packaging/macos/package.sh`, `import-cert.sh` |
| Linux x64 (`ubuntu-22.04`, glibc 2.35) | `.AppImage`, `.deb`, `.rpm` (nfpm), `.tar.gz` | `packaging/linux/package.sh` |

Then it writes `SHA256SUMS.txt`, signs it with the release key (`SHA256SUMS.txt.sig`, the
`checksums` job) and creates a **draft** release. Build provenance is attested once the
repository is public.

Secrets, all in the `release` environment and all optional (unsigned with a warning):
Windows through SignPath Foundation (`SIGNPATH_API_TOKEN` plus three `SIGNPATH_*` variables;
not applied for yet, steps in `docs/CODE_SIGNING.md`), or a certificate of your own through
`sign.ps1` (`WINDOWS_CERTIFICATE`/`_PASSWORD` or six `AZURE_*` values); macOS
`APPLE_CERTIFICATE`/`_PASSWORD` and, for notarisation, `APPLE_ID`, `APPLE_PASSWORD`,
`APPLE_TEAM_ID`; updates `VEIL_UPDATE_KEY`. **Release key, made 17/11/2026** with
`tools/release-key.sh`: the private key is on Jeremy's PC at `~/.veil/release-key.pem` (keep
an offline copy) and in the `VEIL_UPDATE_KEY` secret; the public half is
`crates/veil-app/release-key.pub`. A tag without the secret fails, because every copy from
0.1.2 on refuses an unsigned release. Actions are pinned to SHAs, in
`ci.yml` too from 16/11/2026 (second audit, L1).

Going public (16/11/2026): no secrets in history; workflows hold no secrets and give a
read-only token. `SECURITY.md` asks for private reports. Commits from this checkout use the
GitHub noreply address from 16/11/2026. After
the switch: turn on private vulnerability reporting, protect `main`, and keep approval
required for workflow runs from outside contributors.

1. Bump `version` in the workspace `Cargo.toml`, run the tests, commit and push to `main`.
   The tag must equal the version or the build fails.
2. Optional dry run, publishes nothing: `gh workflow run release.yml --ref main`, then fetch
   the packages from the run's artifacts.
3. Tag and push (needs Jeremy's yes): `git tag vX.Y.Z` then `git push origin vX.Y.Z`.
4. Read the draft on GitHub, then Publish. Only a published, non-pre-release release is seen
   by the app's update check, so publishing is what tells every copy of Veil.
5. Check from outside: `sha256sum -c SHA256SUMS.txt` passes, and with the release key set,
   `openssl pkeyutl -verify -pubin -inkey <public.pem> -rawin -in SHA256SUMS.txt -sigfile
   SHA256SUMS.txt.sig` reports a verified signature.

The update check (`veil-app/src/update/`) asks
`api.github.com/repos/Jeremy8776/veil/releases/latest` once per start.
`cargo test -j 8 -p veil-app update -- --ignored --nocapture` makes the real request.
From 17/11/2026 a copy installed from the MSI, the DMG or the AppImage shows "Install" next to
"Update available" (when the build has a release key): it fetches `SHA256SUMS.txt` and
`.sig` from the release, checks the signature against `release-key.pub` built into the app,
downloads its own package, checks its SHA-256, then runs `msiexec` (Windows, then Veil
closes), copies `Veil.app` out of the DMG (macOS, then reopens), or renames the AppImage over
itself (Linux, then restarts). Portable zip, deb, rpm and tar.gz copies keep the link only.
Not yet run end to end: it needs a published release with a signature. Test on the release
after the key is set: install the older MSI, DMG and AppImage, then click Install.

## Licence note

Run `python tools/licences.py --check` after any `Cargo.lock` change; it regenerates nothing
and fails on a disallowed licence or a stale `docs/DEPENDENCIES.md` (rewrite it without
`--check`). Veil is MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, set once in the
workspace `Cargo.toml`; each crate has `license.workspace = true`).

`openmls_rust_crypto` and `openmls_basic_credential` are not used. The first pulls `hpke-rs`
(MPL-2.0). `veil-group/src/provider.rs` is Veil's own backend on RustCrypto plus `hpke` 0.15
(MIT or Apache-2.0), with one ciphersuite. Re-check licences when bumping openmls.

RustCrypto crates move in generations (digest, hybrid-array, rand_core); one bumped alone does
not build. Since 16/11/2026 Veil is on the digest 0.11 generation (chacha20poly1305 0.11, hkdf
0.13, hmac 0.13, sha2 0.11, ed25519-dalek 3, hpke 0.15, rand_core 0.10 only) and Dependabot
groups the set into one PR (`rustcrypto` in `.github/dependabot.yml`). Upgrade them together and
keep the known-answer tests green (`veil-security/tests/known_answers.rs`,
`veil-group/tests/crypto_known_answers.rs`): they pin wire bytes. The one change hpke 0.15
brought is that X25519 private keys are stored clamped (RFC 9180 section 7.1.2); same key, and
private keys never leave the device. OS randomness is `getrandom::SysRng`.

## Open shortcuts (each marked `shortcut:` in the code)

- Relay sessions are capped at 16 Mbit/s each and the server at 1 Gbit/s, shared fairly by
  address block when short (`veil-rendezvous/src/limit.rs`). People behind one carrier-grade
  NAT address count as one sender.
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
- `veil-app` converts camera and decoded pictures to RGBA on the CPU (`convert.rs`). Screen
  sharing says it is not built yet.
- `veil-app` plans on assumed capacities, by people count alone (`engine/plan.rs`).
- `veil-app`'s own forwarder is reachable on the host's network only, and the host's uplink
  carries every stream to every member. Set `VEIL_FORWARDER` for large internet calls
  (`engine/host.rs`).
- `veil-app` asks for two sizes only, small or everything (`engine/wanted.rs`). A tile that
  shrinks holds its last large picture for about a second before the small layer takes over
  (`engine/video_in.rs`).
- `veil-app` applies routes on the session task, so media pauses while a new member's link
  is dialled (`engine/host.rs`).
- The jitter buffer catches up by skipping packets, which clicks; time-stretch instead if it
  is audible (`veil-media/src/jitter.rs`).
- A lost audio device is an error and the app opens it again, and an open default device
  does not follow a change of Windows' default (`veil-media/src/ffi/windows/audio.rs`). No echo cancellation of Veil's own
  where the device has none.
- Router ports (`veil-portmap/src/lib.rs`): IPv6 pinholes by PCP only (no UPnP IGDv2
  pinholes), and no IPv6 router lookup on macOS (`gateway6.rs`); a second NAT is spotted only from a
  private or shared router address, not by comparing with a rendezvous's reflection; a UPnP
  router that allows permanent mappings only keeps one after a crash.
- H.264 decoding is on the CPU; the GPU path needs a D3D11 device manager
  (`veil-media/src/ffi/windows/decoder.rs`).

## Not yet tested

- The automatic step from a failed punch to the relay. On localhost a punch always succeeds,
  so the relay path is tested through relay-only mode, and the failed direct dial falls back
  to the punch. Covered properly by the two-machine test.

- A commit forged by a non-host member (the `NotFromHost` check) has no test, because openmls
  gives no easy way to build one without a second committer. In `veil-call` only the host
  sends commits, so this needs a deliberately broken host to test.

## Running the two-machine test

Firewall first (found 16/11/2026, first two-machine run of `veil-app` failed to connect):
Windows blocks inbound UDP for a program with no rule, and this PC's Wi-Fi is a Public
network. `veil-probe.exe` and `veil-node.exe` have Allow rules; `veil-app.exe` had none. Add one
on each machine (admin PowerShell, path to that machine's build):
`New-NetFirewallRule -DisplayName "Veil app" -Direction Inbound -Program <path>\target\release\veil-app.exe -Protocol UDP -Action Allow -Profile Private,Public`.
A guest that cannot reach the host now says the host's firewall may be blocking Veil.
Users do not type that command: the host's pre-join screen checks the rules in the background
(`firewall.rs`, about 0.7 s of PowerShell) and, if none lets guests in, shows "Allow guests",
which adds the same rule (name "Veil") through the Windows administrator prompt, then checks
again. Only the host is checked: a guest's own links work without a rule, because hole
punching makes its traffic look like replies. Tested by hand 16/11/2026: Allow guests added
the rule on the host and a guest then connected.
`cargo test -j 8 -p veil-app firewall -- --ignored --nocapture` reads the rules only.
A public `veil-node` (`VEIL_RENDEZVOUS`) removes the need for any rule, since every flow then
starts outbound. The MSI (from 17/11/2026) adds the same rule for the installed
`veil-app.exe` (`packaging/windows/veil.wxs`, WiX's firewall extension, one rule named "Veil" for
Private and Public) and removes it on uninstall, so the button does not show there.
Tested by hand 17/11/2026: install, upgrade over an older MSI, and uninstall left nothing
behind (folder, Start menu entry, firewall rule, registry).

1. Public box: `veil-node` with rendezvous, relay and forwarder on (see the example TOML).
   It prints `rendezvous KEY@IP:PORT` and `forwarder KEY@IP:PORT`. It prints the listen address
   (`0.0.0.0`), so replace that with the box's public IP. Open 4433/udp and 4434/udp.
2. Home network A: `veil-probe host --rendezvous KEY@IP:PORT`. Add `--advertise PUBLIC_IP:7710`
   if port 7710/udp is forwarded to this machine. It prints an invite link.
3. Home network B: `veil-probe join LINK`. Check the safety numbers match on both sides.
4. Read the `link to ...` lines: `Punched` means hole punching worked. Run again with
   `join LINK --relay-only` to test the relay, and with `host --forwarder KEY@IP:PORT` to test
   forwarding. Each invite is one use; the host prints a fresh one after every join.

## 0.1.5 and 0.1.6 (17/11/2026)

v0.1.6 is the published Latest release (tag `v0.1.6` = `4dbffc0`). All 8 packages pass
`sha256sum -c` and the `SHA256SUMS.txt` signature verifies against `release-key.pub`.
Windows and macOS packages are not code-signed. Releases go through a release PR (bump
`Cargo.toml`, `cargo update -w --offline`, CHANGELOG), not an admin bypass.

0.1.5 (`7a2de52`):
- A guest dials a reachable host (#34). The lower device key used to dial every media link;
  when that was the host, it dialled a guest with no open port and the punch timed out, so
  the guest had no sound, video, name or chat (log: "cannot reach <guest>: rendezvous did
  not answer in time"). `routes::dials` now picks the dialler from the shared plan: the
  member without a known address dials the one with one, otherwise the lower key.
  `Routes::Mesh` carries whether this member is reachable. Both ends need 0.1.5 or later.
- Calmer Home (#35): no "Veil" title, content centred. The end screen and Settings >
  Connection match.
- Log lines (#36): the host logs "call: <key> joined" and "call: <key> left", and
  "call: linking up took ..." when over a second. New tests: a guest leaving is seen by the
  host, and the host leaving ends the guest's call (`veil-call/tests/session.rs`, loopback).
- Invite time left (#37, `screens/invite.rs`, `engine/invites.rs`): "Expires in 42 min" on
  the invite card, People and the Invite tooltip. A link that runs out unused is replaced.
  "Keep links working until the call ends" makes 30-day links; the call ending still stops
  them, because the lobby lives in the host's memory. Each link still admits one person.

0.1.6 (`4dbffc0`):
- Devices during a call (#38): the arrow beside Mute picks a microphone or speaker and sets
  microphone (0 to 200%) and speaker (0 to 100%) volume; the arrow beside Stop video picks a
  camera. The pickers are shared with pre-join (`widgets.rs`). A new speaker reaches the
  playback thread through `Playback` (`volume.rs`) and reopens on the next 20 ms frame.
- Stats overlay (#38), off by default: the signal button in the top bar or Ctrl+Shift+D.
  Per person: path, QUIC round trip, loss, bit rate down and up, received video size and
  frame rate, received sound level; your microphone level; a forwarder row.
  `MediaSession::link_stats` reads QUIC counters only, so nothing new is sent.
- Device log (#39): each microphone, camera or speaker open (or failure) is logged as chosen
  or system default, without names or ids.

Open from these releases:
- **Leaves go unnoticed in calls across networks** (host or guest leaving). Cause unknown
  and not confirmed by logs. On one machine both directions work. Ruled out: the host's
  goodbye being lost at runtime shutdown. Suspect, unconfirmed: dials in
  `MediaSession::apply` hold the call loop (the `engine/host.rs` shortcut). Next: a call
  with Martin, both on 0.1.6; collect both `%LOCALAPPDATA%\Veil\logs\veil.log` files and
  note which side left. Look for "call: <key> left", "call: linking up took" and
  "cannot reach".
- Not yet tested by hand: sound, video, names and chat across the internet with Martin
  (#34); changing devices mid-call and the stats overlay (#38); the invite timer and
  checkbox; the self-update Install button; Settings > Connection; the white icon on light
  backgrounds; the reachability check across the internet (needs a public `veil-node`); CPU
  use while Home is open; the new Home layout.

## Direction (agreed 17/11/2026)

The premise, so no session drifts from it: a Zoom or Teams style app where calls are
peer-to-peer by default, and the user may add servers of their own (rendezvous, relay,
forwarder) whenever they want. Every role stays a module behind one interface
(ARCHITECTURE.md section 1). Direct calls usually need a reachable host, so opening a port
must be as easy as possible without weakening security.

Decisions:
- **Port opening: automatic, opt-in.** Veil asks once, then maps the call's UDP port and, when
  the host runs its own rendezvous, that rendezvous's port on the router with PCP, then NAT-PMP, then UPnP IGD. The mapping is for the call
  only: a short lease, renewed while the call runs, deleted when it ends. If the router
  refuses, Veil shows a written guide and a reachability check. If the router's external
  address differs from the address a rendezvous reflects, the user is behind carrier-grade
  NAT and Veil says a public node is needed: forwarding cannot work there.
- **Default node: optional.** Veil may ship with the address of a public `veil-node` that
  Jeremy runs. It is off until the user turns it on, and TRANSPARENCY.md says what it sees
  (IP addresses, timing, room hashes; never media). The relay's total cap and fair sharing
  this needed are done (Open shortcuts).
- **Servers are chosen in the app**, saved per device. Environment variables still override.
- `design/` (icon concepts) stays out of git. From 17/11/2026 the icon is concept 33b without
  its teal square, the V cut out, in plain white. `tools/make-icons.py` (needs Pillow) writes
  `assets/veil-logo.svg` (README), `veil-256.png` and `veil.ico`; `src/logo.rs` draws the
  same mark live on Home with moving waves. Change both together.

Work, in order:
1. Done 17/11/2026: Settings > Connection ("Connection" on Home; `screens/connection.rs`,
   `connection.rs`). Rendezvous, forwarder, public addresses and a fixed port, saved to
   `%LOCALAPPDATA%\Veil\connection.json` only when every field checks (`plan::parse_node`,
   `parse_advertise`, `parse_port`; `0.0.0.0` and port 0 are refused). Each set `VEIL_*`
   variable still wins, and the screen says so. A bad file is reported, ignored, and moved to
   `connection.json.bak` on the next Save. Not yet run by hand.
2. Done 17/11/2026: DNS names for configured nodes (`veil_net::NodeName`, `KEY@HOST:PORT`).
   Settings and `VEIL_RENDEZVOUS` / `VEIL_FORWARDER` take a name; `host.rs` and `guest.rs`
   resolve it when the call starts (`plan::resolve`, 5 s limit, IPv4 first, `0.0.0.0` and
   `::` refused), so invites and plans still carry literal addresses. A spoofed name can only
   stop a call: the pinned key still refuses any other server. `veil-probe` still takes IPs.
3. Done 17/11/2026, except the reachability check: port opening (`crates/veil-portmap`,
   `veil-app/src/engine/router.rs`). Asked once on the host's pre-join screen ("Open a port
   on your router?", Allow or Not now), saved as `open_port` and `asked_open_port` in
   `connection.json`, and a checkbox in Settings > Connection. While hosting it maps the
   call port (PCP, then NAT-PMP if the router answers PCP with "unsupported version", then
   UPnP), 20-minute lease renewed at half, removed on leave within 0.8 s. The public address
   goes first in a fresh invite. People shows "Router port open" or "Router did not open a
   port" with the guide (forward the port by hand, or a public node for carrier-grade NAT) in
   the tooltip. `cargo test -j 8 -p veil-portmap real_router -- --ignored --nocapture` tries
   this network's router. Run 17/11/2026 on Jeremy's network: gateway 192.168.1.1 found, no
   PCP answer, no UPnP answer (Windows' own UPnP finder also sees no gateway device), so the
   router had these off. After Jeremy turned on UPnP in the router (17/11/2026), the test
   passed: NAT-PMP mapped the port, renewed it and closed it. Not yet tried in a real call.
   Reachability check done 17/11/2026 (branch `feat/reachability`, stacked on
   `feat/port-mapping`): `ClientMsg::CheckReach { port }` asks a rendezvous to dial the
   asker's own reflected IP at that port from a fresh socket and throwaway key, pinned to the
   asker's key (`veil-rendezvous/src/reach.rs`; 8 per link, one at a time, 32 per server, 5 s
   each). Sent on a link of its own (`check_reach`), because an older node closes a link on a
   message it does not know. The app (`engine/reach.rs`) checks the router's address once
   it opens, or the first public address set by hand, but only with a configured public
   node. People shows "Reachable from the internet" or "Not reachable from the internet";
   if the node sees another IP than the router's, the tooltip says the provider shares one
   address (carrier-grade NAT) and guests come through the relay. Tested on localhost only.
   shortcut: the dial comes from the node's own IP, so a NAT that filters by IP only can
   pass it without an open port.
4. The two-machine internet test (below), with and without a mapped port.
5. The optional default node.
Video filters (#21) and background blur (#28) were merged and shipped in 0.1.4.

IPv6 (done 17/11/2026, branch `claude/ipv6-first-7000b2`): participants bind one dual-stack
socket (`Node::bind_any`; `[::]` is dual-stack on every system). The host's public IPv6
address (`veil_net::family::global_ipv6`, the source address the routing table picks, often a
temporary one) goes in invites after configured addresses; members send theirs to the
rendezvous in `Register`'s existing `local_addrs`, so no wire change and 0.1.5/0.1.6 still
interoperate. `Node::connect` dials IPv6 first and IPv4 after 250 ms (`family::dial_order`).
With port opening on, `veil_portmap::Pinhole` asks the IPv6 router by PCP (none answered on
Jeremy's network, as for IPv4); with a public node that has IPv6 (`NodeName::resolve_ipv6`),
`engine/reach.rs` checks the IPv6 address too. Call stats and `veil-probe` link lines say
"over IPv6". Tested on one machine only: `veil-probe` host and joiner linked Direct over IPv6,
and an IPv4-only joiner linked over IPv4. For a public `veil-node` to reflect and check IPv6,
set `listen = "[::]:4433"`. Not yet tried between two real IPv6 networks.

## Next, in suggested order

From 17/11/2026, first:
- A call with Martin on 0.1.6 to find why leaves go unnoticed (logs from both sides), and
  to test #34 and #38 across the internet. See "0.1.5 and 0.1.6" above.
- The hand tests listed there.
- Later: the optional default node. Calls of three or more where no guest has an open port still need a rendezvous.

Earlier list:

1. Two-machine test over the real internet with `veil-probe` (steps above). Needs Jeremy's
   machines and a public box.
2. `veil-media`, continued: the core, keyframe requests, the Windows H.264 encoder and
   Windows camera capture, Opus, the H.264 decoder, the jitter buffer and WASAPI are done
   (see above). Windows media is complete for a first call. Not yet run: the microphone test
   (`--ignored`). macOS and Linux back ends come later. Decided
   15/11/2026: `unsafe` FFI and libopus are allowed in `veil-media` only, under the rules in
   `docs/TRANSPARENCY.md` section 5 (one `ffi` module per OS, `// SAFETY:` on every block,
   `clippy::undocumented_unsafe_blocks` denied, each module listed in that table).
3. `veil-app`: wired to real calls (see above). Next: Jeremy runs two copies on this machine
   (with headphones): New call in one, copy the invite from People, Join in the other, Admit.
   Names and chat are sent (16/11/2026). A forwarder carries calls of 5 or more
   (16/11/2026), asking for layers by tile size. Then: a 5-person test.
