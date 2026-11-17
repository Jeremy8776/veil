# Changelog

All notable changes to Veil. Dates are dd/mm/yyyy.

## Unreleased

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
