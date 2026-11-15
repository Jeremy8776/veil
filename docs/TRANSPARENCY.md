# Transparency

Veil is open source. This page says plainly what it depends on, where it uses code Rust cannot
check, what it does not protect, and what is not finished. If something here is wrong or out of
date, that is a bug.

## 1. Status

Veil is early and unaudited. Do not trust it with anything that matters yet.

| Built and tested on localhost | Planned, not built |
|---|---|
| Device keys, safety numbers, signed invites | Capture, encoding and decoding (`veil-media` has its schedule and framing only) |
| MLS group keys, the host's lobby | The desktop app (`veil-app`) |
| Per-frame encryption with a layer marker | Signed organisation policy, OIDC or SAML identity |
| QUIC links pinned to device keys | Metrics export (OpenTelemetry) |
| Rendezvous, hole punching, relay | Compliance recording, federation, gateways |
| Forwarder with layer subscriptions | Padding against size analysis |
| `veil-node`, `veil-probe` | An independent security audit |

[ARCHITECTURE.md](ARCHITECTURE.md) describes the whole design, including the planned parts.
[HANDOFF.md](HANDOFF.md) says what is done and what is next.

## 2. What other parties can see

Media is encrypted on the sender's device and opened only on members' devices. Relays,
forwarders and rendezvous nodes never hold media keys. They can still see this:

| Party | Sees |
|---|---|
| The network | Which addresses talk to which, when, and how much |
| Rendezvous node | Members' IP addresses, a hash of the room id, when people join |
| Relay | Members' IP addresses, encrypted traffic volume and timing |
| Forwarder | Members' device keys and the room hash (from the host's roster ticket), traffic volume and timing, and each video frame's layer marker: which resolution tier and frame-rate step it is, and whether it is a keyframe |
| Other members | Your device key, and your IP address unless you use relay-only mode |

**Known leaks:**
- Packet sizes and timing show when someone speaks or moves. Padding is planned, not built.
- Layer markers show which tiers each sender sends and each receiver asks for, so a forwarder
  can tell whose tile is large on someone's screen.
- Keyframe requests are not authenticated end to end. A forwarder or member can ask a sender
  for keyframes at will. That costs the sender bandwidth, but no more than one keyframe per
  500 ms, and it reveals nothing.

## 3. No telemetry, no default servers

Veil sends nothing to its authors and has no built-in servers. Every rendezvous, relay and
forwarder address comes from an invite or a config file you control. Addresses in the source
code are from the documentation ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24,
2001:db8::/32) and appear only in tests.

## 4. Dependencies and licences

- Veil itself is licensed under either of [MIT](../LICENSE-MIT) or [Apache-2.0](../LICENSE-APACHE),
  at your option, like most of the Rust ecosystem. Apache-2.0 adds an explicit patent grant.
- Every third-party crate, its version and its licence is listed in
  [DEPENDENCIES.md](DEPENDENCIES.md). It is generated from `Cargo.lock` by
  `python tools/licences.py`, which also fails if a crate's licence is not allowed. Run it with
  `--check` before each commit that changes `Cargo.lock`.
- Only permissive licences are allowed (MIT, Apache-2.0, BSD, ISC, Zlib and similar). Copyleft
  licences (GPL, LGPL, MPL) are kept out so anyone can reuse any part of Veil. For this reason
  Veil writes its own MLS crypto backend instead of using `openmls_rust_crypto` (which pulls in
  MPL-2.0 code), and does not use FFmpeg (LGPL).

## 5. Code Rust cannot check

**Today:** the workspace sets `unsafe_code = "forbid"`, so the compiler refuses `unsafe` in
every crate except `veil-media`. That crate sets `deny` instead, and only its Windows `ffi`
module allows it. The rest of `veil-media` is safe Rust.

**Native code in dependencies:** `ring` (used by `rustls` and `quinn` for TLS) is written partly
in C and assembly. It is licensed Apache-2.0 and ISC.

**In `veil-media`:**
- Camera and screen capture and hardware video encoding are only available through each
  operating system's own APIs: Media Foundation (Windows), VideoToolbox (macOS), VA-API (Linux).
  Calling them needs `unsafe` foreign-function calls. These libraries ship with the operating
  system; Veil does not bundle them.
- Opus audio means the libopus C library (BSD-3-Clause), built from source.
- Rules for that code:
  - `unsafe` is allowed only in `veil-media`, and only inside one `ffi` module per platform.
    Every other crate keeps `forbid`.
  - Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
    `clippy::undocumented_unsafe_blocks` is set to deny, so a missing comment fails the build.
  - Every `unsafe` module is listed in the table below, with what it calls.

| Module | Calls | Why |
|---|---|---|
| `veil-media/src/ffi/windows` (`mod.rs`, `enumerate.rs`, `sample.rs`) | COM start-up (`CoInitializeEx`, `CoCreateInstance`, `CoTaskMemFree`); Media Foundation (`MFStartup`, `MFTEnumEx`, the H.264 encoder transform `IMFTransform` and its `ICodecAPI` settings, media types, samples and buffers) | Hardware H.264 encoding on Windows, one encoder per simulcast rung, with Microsoft's software encoder as the fallback. Both ship with Windows |
| `veil-media/src/ffi/windows/camera.rs` | Media Foundation capture (`MFEnumDeviceSources`, the camera's `IMFMediaSource`, `MFCreateSourceReaderFromMediaSource`, `IMFSourceReader::ReadSample`, `IMF2DBuffer2::Lock2DSize`) | Camera pictures on Windows, converted to NV12 by Windows' own video processing. Each picture is copied out of the locked buffer, within the bounds Windows reports, before it is used. The camera's name stays on the device; nothing about it is sent |

## 6. Known shortcuts

Where the code takes a known shortcut, it says so in a comment that starts with `shortcut:`,
with the limit and when to fix it. To list them all:

```bash
grep -rn "shortcut:" crates
```

[HANDOFF.md](HANDOFF.md) keeps the same list in plain English.

## 7. Not yet tested

- Hole punching across real home routers, and the automatic step from a failed punch to the
  relay. Every test so far runs on one machine.
- A forged MLS commit from a member who is not the host. The check exists; there is no test,
  because the MLS library gives no easy way to build such a commit.
- Any test on macOS or Linux. Development so far is on Windows.
- Live camera capture runs only on request (`cargo test -p veil-media --test camera --
  --ignored`), because it turns the camera on. Listing cameras runs in every test pass.

## 8. Reporting a security problem

There is no private reporting channel yet. One will be set up before the first public release.
