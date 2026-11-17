# Changelog

All notable changes to Veil. Dates are dd/mm/yyyy.

## 0.1.1, 17/11/2026

Security fixes. Update every device in a call: see the upgrade note below.

### Security

- One sealed packet from a member can no longer block another member's names and chat, on any
  route, or their video and sound on a direct link. Frames on a direct link must come from the
  member they claim, and the names and chat window moves only after the device signature
  checks out. (GHSA-rvhv-r5mr-2w3h)
- A name or chat line signed in an earlier call can no longer be replayed into the current call.
  The signature now covers a value unique to the call and epoch. (GHSA-qqx7-3x2m-v277)
- One forged audio packet with a far-ahead sequence number no longer silences a member at a
  receiver. (GHSA-9wq4-7r8v-7582)
- A member that withholds QUIC stream credit can no longer stall another participant's call
  loop, or block removal on the host. No media send waits on one peer without a deadline.
  (GHSA-37f5-22p2-q6hr)
- `veil-node` forwarder: queued video is bounded per node and per source, and stuck frames
  expire. One device key can no longer use up the node's memory. (GHSA-vrpc-82hc-76gf)

Still open: through a forwarder, a member can block another member's video and sound until the
next epoch. See docs/TRANSPARENCY.md.

**Upgrade note:** signed names and chat use message format version 3. Devices on 0.1.1 and
devices on 0.1.0 ignore each other's names and chat.

### Fixes

- Guest join shows "Reaching the host" until the knock lands, and Leave works at once.
- Icons are drawn in code where the system has no icon font (Linux).
- Microphone and speakers survive audio glitches (xruns), device reroutes and a refused
  real-time priority instead of stopping for the rest of the call.

### Linux and macOS

Linux and macOS builds now have camera, microphone, speakers and video. Not yet tried on real
Linux or macOS machines.

- Linux: camera through Video4Linux2 (NV12, YUYV or MJPEG), microphone and speakers through ALSA.
- macOS: camera through AVFoundation, microphone and speakers through Core Audio. macOS asks for
  camera and microphone permission the first time.
- H.264 on Linux and macOS through OpenH264, built from source. Calls work across Windows, Linux
  and macOS. Cisco's H.264 patent cover does not apply to a build from source; see
  docs/TRANSPARENCY.md.

## 0.1.0, 16/11/2026

First public release. Windows only. Not independently audited.

- Desktop app: start or join a call, lobby with admit and decline, camera and microphone
  check, names, chat, safety numbers, a demo call.
- End-to-end encryption of video, sound, names and chat. MLS group keys, a new epoch on every
  join or leave.
- Names and chat are signed by the sender's device key, so no member can speak as another.
- A removed member's old key stops working at once.
- Direct links, hole punching, relay fallback and forwarders, planned per call.
- `veil-node` for running your own rendezvous, relay and forwarder.
- An update check that only notifies. It never downloads anything.
