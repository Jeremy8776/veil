# Transparency

Veil is open source. This page says plainly what it depends on, where it uses code Rust cannot
check, what it does not protect, and what is not finished. If something here is wrong or out of
date, that is a bug.

## 1. Status

Veil is early and unaudited. Do not trust it with anything that matters yet.

| Built and tested on localhost | Planned, not built |
|---|---|
| Device keys, safety numbers, signed invites | Capture, encoding and decoding on macOS and Linux |
| Windows camera capture, H.264 encoding and decoding, Opus audio | Veil's own echo cancellation (Windows' is used where the device offers it) |
| MLS group keys, the host's lobby | Raised hands sent between members (yours shows only on your screen) |
| Per-frame encryption with a layer marker | Signed organisation policy, OIDC or SAML identity |
| QUIC links pinned to device keys | Metrics export (OpenTelemetry) |
| Rendezvous, hole punching, relay | Compliance recording, federation, gateways |
| Forwarder with layer subscriptions | Padding against size analysis |
| `veil-node`, `veil-probe` | An independent security audit |
| The desktop app in a real call: host, lobby, guest, video and sound both ways, names and chat | |

[ARCHITECTURE.md](ARCHITECTURE.md) describes the whole design, including the planned parts.
[HANDOFF.md](HANDOFF.md) says what is done and what is next.

## 2. What other parties can see

Media, display names and chat are encrypted on the sender's device and opened only on
members' devices. Relays, forwarders and rendezvous nodes never hold these keys. They can still
see this:

| Party | Sees |
|---|---|
| The network | Which addresses talk to which, when, and how much |
| Rendezvous node | Members' IP addresses, a hash of the room id, when people join |
| Relay | Members' IP addresses, encrypted traffic volume and timing |
| Forwarder | Members' device keys and the room hash (from the host's roster ticket), traffic volume and timing, and each video frame's layer marker: which resolution tier and frame-rate step it is, and whether it is a keyframe. Names and chat pass through it sealed: it sees when one is sent and how large it is, not who sent it or what it says |
| Other members | Your device key, the display name you chose, your chat messages, and your IP address unless you use relay-only mode. Chat is not stored: it is gone when the call ends |
| Anyone holding an invite link | The host's device key and the addresses in it: the host's LAN address and port, and any public address set in `VEIL_ADVERTISE`. The desktop app puts these in every invite so a guest can dial directly. Through the rendezvous they can also learn the host's public address, since joiners punch through to the host. They cannot learn any other member's address: a member accepts an introduction only from peers its current routes link to |

When the desktop app hosts without a configured `veil-node`, it runs a rendezvous, relay and
forwarder of its own on extra ports for the length of the call. They see what any rendezvous,
relay or forwarder sees (the table above), and they are the host's own machine.

The rendezvous passes on a member's address only after that member agrees. A request for an
introduction or a relay session goes to the member first, and the member's app agrees only
if its current routes expect the one asking (the host agrees to anyone holding the invite).
A relay-only member never agrees to an introduction.
The asker gets the same silence whether the member declines or is not there, so the
rendezvous never says who is online.

**Kept on your device** (desktop app, Windows, in `%LOCALAPPDATA%\Veil`):
- `device.key`: your device key. The same key is used for every call, so a safety number
  you checked once still holds next time. Sealed with Windows DPAPI, so the file opens only
  for your Windows user; a copy on another computer gives nothing. Anyone who controls your
  signed-in account can still use it. "New device key" on the home screen replaces it.
- `verified.keys`: the device keys you marked as verified, sealed the same way. Verified is
  a label for the host; it never lets anyone in.
- The same key in every call means anyone who meets you in two calls can tell it is the same
  device. Before 16/11/2026 every call used a new key.

**Known leaks:**
- Packet sizes and timing show when someone speaks or moves. Padding is planned, not built.
- Fixed 16/11/2026: Opus now runs at a constant bitrate, so every voice packet is the same
  size (80 bytes at 32 kbit/s) whatever is said. Variable-bitrate packet sizes can reveal some
  words even when encrypted. When a packet is sent still shows when someone has their
  microphone on.
- Layer markers show which tiers each sender sends and each receiver asks for, so a forwarder
  can tell whose tile is large on someone's screen.
- A display name is whatever its owner typed. Veil does not check it, so someone can call
  themselves by another person's name. When two people in a call show the same name (case
  and spacing ignored), each gets the start of their device key after it, so they never look
  alike. Letters that only look the same, such as Latin "a" and Cyrillic "а", are not caught.
  Who someone is rests on the safety number; the host badge comes from the invite's host
  key, not from a name.
- Chat message sizes are not padded, so a relay or forwarder sees roughly how long each
  message is.
- Keyframe requests are not authenticated end to end. A forwarder or member can ask a sender
  for keyframes at will. That costs the sender bandwidth, but no more than one keyframe per
  500 ms, and it reveals nothing.

## 3. No telemetry, no default servers

Veil sends nothing to its authors and has no built-in servers. Every rendezvous, relay and
forwarder address comes from an invite, a config file or an environment variable you control
(`VEIL_RENDEZVOUS`, `VEIL_ADVERTISE`, `VEIL_PORT` for the desktop app). To find its LAN
address, the app asks the operating system which interface would route to a documentation
address (192.0.2.1); no packet is sent. Addresses in the source
code are from the documentation ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24,
2001:db8::/32) and appear only in tests.

**Your firewall.** On Windows, before you host, the app reads the firewall rules (PowerShell,
`Get-NetFirewallApplicationFilter`, read only) to see whether guests can reach it. If not, it
shows an "Allow guests" button. Only that button changes anything: it runs
`netsh advfirewall firewall add rule name="Veil"` through Windows' administrator prompt, which
you confirm or decline. The rule allows inbound UDP to this `veil-app.exe` only, on Private and
Public networks. Remove it in Windows Defender Firewall (Inbound Rules, "Veil") at any time.
The code is `crates/veil-app/src/firewall.rs`.

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
in C and assembly. It is licensed Apache-2.0 and ISC. The desktop app (`veil-app`) draws with
`eframe`, `winit` and `wgpu`, which call the operating system's window and graphics APIs
(DirectX 12 or Vulkan on Windows) through their own `unsafe` code; Veil's own app code has none.

**Fonts and icons:** none are bundled. `veil-app` reads the system's own UI, symbol and icon
fonts at start-up (Segoe UI and Segoe Fluent Icons or Segoe MDL2 Assets on Windows). egui's
bundled fonts are turned off because their licences are outside the allow list.

**In `veil-media`:**
- Camera and screen capture and hardware video encoding are only available through each
  operating system's own APIs: Media Foundation (Windows), VideoToolbox (macOS), VA-API (Linux).
  Calling them needs `unsafe` foreign-function calls. These libraries ship with the operating
  system; Veil does not bundle them.
- Opus audio means the libopus C library (BSD-3-Clause). Its sources are in
  `crates/veil-media/vendor/opus` (version, origin, what was left out, and a file-by-file
  check against xiph.org's release tarball in `VENDORED.md`),
  and `build.rs` compiles them with the `cc` crate. The deep-learning features are not built.
- Rules for that code:
  - `unsafe` is allowed only in `veil-media`, and only inside one `ffi` module per platform,
    plus `ffi/opus.rs` for libopus.
    Every other crate keeps `forbid`.
  - Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
    `clippy::undocumented_unsafe_blocks` is set to deny, so a missing comment fails the build.
  - Every `unsafe` module is listed in the table below, with what it calls.

| Module | Calls | Why |
|---|---|---|
| `veil-media/src/ffi/windows` (`mod.rs`, `enumerate.rs`, `sample.rs`) | COM start-up (`CoInitializeEx`, `CoCreateInstance`, `CoTaskMemFree`); Media Foundation (`MFStartup`, `MFTEnumEx`, the H.264 encoder transform `IMFTransform` and its `ICodecAPI` settings, media types, samples and buffers) | Hardware H.264 encoding on Windows, one encoder per simulcast rung, with Microsoft's software encoder as the fallback. Both ship with Windows |
| `veil-media/src/ffi/windows/decoder.rs` | Microsoft's H.264 decoder transform (`CoCreateInstance` of `CLSID_MSH264DecoderMFT`, `IMFTransform`, `ICodecAPI` low latency, media types, `GetBlob` for the display area) and the shared NV12 copy in `sample.rs` | Decoding other members' video on Windows, on the CPU. The bitstream comes from another member and is parsed by Windows' decoder, so that decoder is part of the attack surface. Veil refuses any decoded size above 4K before allocating for it, and copies each picture out of the locked buffer within the bounds Windows reports |
| `veil-media/src/ffi/windows/audio.rs`, `audio_devices.rs` | WASAPI (`IMMDeviceEnumerator` `EnumAudioEndpoints`, `GetDevice`, `GetDefaultAudioEndpoint`, each device's `GetId` and friendly name through `OpenPropertyStore`, `PropVariantToStringAlloc`, `PropVariantClear`, `CoTaskMemFree`, `IAudioClient` and `IAudioClient2` in shared, event-driven mode, `IAudioCaptureClient`, `IAudioRenderClient`, `CreateEventW`, `WaitForSingleObject`, `CloseHandle`) | Microphone and speakers on Windows, 48 kHz float, converted by Windows. Samples are copied byte by byte within the frame count WASAPI gives. The streams use the communications category, so Windows may apply the device's own echo cancellation and noise suppression. Device names and ids stay on this device; nothing about them is sent |
| `veil-media/src/ffi/windows/camera.rs` | Media Foundation capture (`MFEnumDeviceSources`, the camera's `IMFMediaSource`, `MFCreateSourceReaderFromMediaSource`, `IMFSourceReader::ReadSample`, `IMF2DBuffer2::Lock2DSize`) | Camera pictures on Windows, converted to NV12 by Windows' own video processing. Each picture is copied out of the locked buffer, within the bounds Windows reports, before it is used. The camera's name stays on the device; nothing about it is sent |
| `veil-media/src/ffi/windows/protect.rs` | DPAPI (`CryptProtectData`, `CryptUnprotectData` with `CRYPTPROTECT_UI_FORBIDDEN` and a per-purpose label as extra entropy, `LocalFree`) | Sealing `veil-app`'s saved device key and verified list to the signed-in Windows user. It sits in `veil-media` only because all of Veil's `unsafe` code does |
| `veil-media/src/ffi/opus.rs` (every platform) | libopus: `opus_encoder_create`, `opus_encode_float`, `opus_encoder_ctl`, `opus_decoder_create`, `opus_decode_float`, the matching `_destroy` calls, `opus_get_version_string` | Opus audio. Every buffer is a Rust slice whose length is the count passed to libopus, and packet lengths are checked before decoding. Each state is freed once, on drop |

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
  The microphone test is the same (`--test wasapi -- --ignored`); playback of silence runs in
  every test pass. The desktop app's two-engine call (`cargo test -p veil-app loopback --
  --ignored`) opens ports, the H.264 encoder and the speaker, so it also runs only on request.
- A call between two copies of the desktop app with real cameras and microphones has not yet
  been run by a person.

## 8. Reporting a security problem

There is no private reporting channel yet. One will be set up before the first public release.
