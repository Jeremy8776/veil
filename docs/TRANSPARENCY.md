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
[HANDOFF.md](HANDOFF.md) says what is done and what is next. To report a hole, see
[SECURITY.md](../SECURITY.md).

## 2. What other parties can see

Media, display names and chat are encrypted on the sender's device and opened only on
members' devices. Relays, forwarders and rendezvous nodes never hold these keys. They can still
see this:

| Party | Sees |
|---|---|
| The network | Which addresses talk to which, when, and how much |
| Rendezvous node | Members' IP addresses, a hash of the room id, when people join. Each member with IPv6 also tells it its public IPv6 address, so other members can be introduced to it and dial it over IPv6. When a host with a configured `veil-node` has a public port (opened on the router, or set by hand), the node also learns that port: it dials the host back once, from a separate port with a throwaway key, to check guests can reach it. It dials only the asker's own IP. If that node has an IPv6 address too, it does the same for the host's public IPv6 address. While a host's room is open, the node also stores its sealed invites for short links, and sees when each is stored and which IP addresses fetch one. It cannot read them |
| Relay | Members' IP addresses, encrypted traffic volume and timing |
| Forwarder | Members' device keys and the room hash (from the host's roster ticket), traffic volume and timing, and each video frame's layer marker: which resolution tier and frame-rate step it is, and whether it is a keyframe. Names and chat pass through it sealed: it sees when one is sent and how large it is, not who sent it or what it says |
| Other members | Your device key, the display name you chose, your chat messages, and your IP addresses (IPv4 and, if you have one, your public IPv6 address) unless you use relay-only mode ("Hide my address" in Settings > Connection, off by default). It applies only when you join: a host's invite always carries the host's addresses, and the relay still sees yours. Chat is not stored: it is gone when the call ends |
| Your router | With "Open a port on my router" on, while you host: this computer's LAN address, the ports opened for the call, and that a program called "Veil" asked for them (section 3). With IPv6, also this computer's public IPv6 address, in a PCP request to let the call's port through the router's firewall |
| Anyone holding an invite link | The host's device key and the addresses in it: the host's LAN address and port, the router's public address and port when the router opened one, the host's public IPv6 address and port when it has one, and any public address set in `VEIL_ADVERTISE`. If the host runs its own rendezvous, also that rendezvous's LAN address and port, and its public port when the router opened one. The desktop app puts these in every invite so a guest can dial directly. Through the rendezvous they can also learn the host's public address, since joiners punch through to the host. They cannot learn any other member's address: a member accepts an introduction only from peers its current routes link to |

When the desktop app hosts without a configured `veil-node`, it runs a rendezvous, relay and
forwarder of its own on extra ports for the length of the call. They see what any rendezvous,
relay or forwarder sees (the table above), and they are the host's own machine.

**Short invite links** hold the rendezvous's addresses in readable form and a random secret.
Anyone holding one can fetch the invite, so it shows them everything a full link does. The
guest fetches from those addresses with a throwaway key and does not check the server's key,
because the link has no room for it. The invite is encrypted and signed, so a wrong server
cannot read or change it, but whoever answers at those addresses sees the guest's IP address
and can refuse to give the invite. This happens even if the guest set a rendezvous of their
own. "Copy full link" avoids the fetch.

The rendezvous passes on a member's address only after that member agrees. A request for an
introduction or a relay session goes to the member first, and the member's app agrees only
if its current routes expect the one asking (the host agrees to anyone holding the invite).
A relay-only member never agrees to an introduction. A member dials back only for an
introduction it has just agreed to, at most once every 10 seconds for each peer and 16 times
every 10 seconds in all. A dial-back goes to the addresses that peer listed at the rendezvous,
which the rendezvous does not check.
The asker gets the same silence whether the member declines or is not there, so the
rendezvous never says who is online.

A relay limits who can hold its relay sessions. Two members hold at most 4 together, and one
IPv4 address or IPv6 /64 holds at most 128. A session unused for 30 seconds can be taken back,
and a full relay can replace the longest-idle session of the address that holds the most. The
relay does not tell the members when it does this. A link still using a session keeps it unless
the relay is full.

**Screen sharing** starts only when you choose a screen or window and press Share. While it
runs, the call shows what you are sending and a small bar over your other apps says so. On
Windows that bar is kept out of captures, so those watching do not see it. The
picker's thumbnails and preview stay on your device. While the picker is open, and for half a
second after it closes, Veil's window is kept out of captures on Windows. Where that cannot be
done, those watching get a still picture for that time. Pictures are sealed end
to end like your camera, and window titles are neither logged nor sent. A forwarder sees one
more video stream from you but not that it is a screen. On Windows the live share and the
picker's thumbnails use Windows Graphics Capture, which includes the mouse pointer. The first
thumbnails, the area preview and other systems use the `xcap` crate, which holds its own
`unsafe` code. If the source goes away,
sharing stops rather than moving on. On KDE Plasma's Wayland session Veil uses KWin's own
interfaces, which KWin offers to any app that is not sandboxed: a list of open windows with
their titles, screenshots of a screen or window handed over through a pipe in memory, and a
stream of the source you share. So there, as on Windows and X11, Veil can see and capture
windows without the desktop asking; it lists and captures only while the Share picker is open
or you are sharing, and leaves its own windows out. Veil's `.desktop` file names these
interfaces so that older Plasma versions, which ask for that, allow them too. On other Wayland
desktops, and where KWin's interfaces are missing (in a sandbox, say), Veil does not list or
capture anything itself: your desktop's own chooser asks what to share, and Veil receives only
that one picture stream, through the desktop portal and PipeWire (the `ashpd` and `pipewire` crates,
which call the system's PipeWire library). Pictures stay in memory; `xcap` is not used there,
because it would have the desktop write each picture of your screen to a file. The pointer is
drawn in where the desktop offers it. Your desktop's chooser also lists Veil's own window, and
Veil cannot leave it out. Ending the share in Veil, or backing out of the picker, closes the
stream and the desktop's session; stopping it from the desktop ends the share in Veil.

**Filters and Background blur** run on your device, on each camera picture before your
self-view and before it is encoded, so other members receive only the changed picture. Finding
you in the picture is done by a small network built into Veil (see section 4). It runs on a
thread of its own while blur is on, and nothing is sent anywhere to do it.
- Blur is not a privacy guarantee. The network makes mistakes: edges, hair, hands and objects
  you hold up can show what is behind you, most of all when you move or the light is poor.
- If your computer is too busy for the network to keep up, your camera turns itself off and
  Veil says so. The picture that could not be blurred is not sent, and blur stays chosen, so
  your background is never sent sharp unless you turn blur off yourself. Until the first
  result is ready after you turn blur on, the whole picture is blurred.

**Kept on your device** (desktop app, Windows, in `%LOCALAPPDATA%\Veil`):
- `device.key`: your device key. The same key is used for every call, so a safety number
  you checked once still holds next time. Sealed with Windows DPAPI, so the file opens only
  for your Windows user; a copy on another computer gives nothing. Anyone who controls your
  signed-in account can still use it. "New device key" on the home screen replaces it.
- `verified.keys`: the device keys you marked as verified, sealed the same way. Verified is
  a label for the host; it never lets anyone in.
- Veil never writes over a saved file it could not open. One that does not open for your
  user is kept as `*.unreadable` (then `.unreadable.2` and on); one that cannot be read at
  all, for example while it is locked, is left as it is, and that run's key or list is not
  saved.
- The same key in every call means anyone who meets you in two calls can tell it is the same
  device. Before 16/11/2026 every call used a new key.
- `no-update-check`: present only if you turned the update check off.
- `updates\`: the last update you chose to install from the MSI, kept until the next one.
- `logs\veil.log` and `logs\veil.previous.log` (from 0.1.3, `veil-app/src/log.rs`): this run's
  log and the last run's, for you to send when something goes wrong. "Logs" on the home
  screen opens the folder. Nothing reads or sends them but you. They hold the version, call
  states (connecting, lobby, live, ended and why), timings, device and encoder errors, the
  error that stopped Veil if it could not start or run, and error text from the network,
  which can include a server's or peer's IP address. People
  appear at most as the first 8 hex digits of their device key: no names, chat, invite links
  or keys. Each file stops at 5 MB. They are plain text, not sealed. On macOS they are in
  `~/Library/Logs/Veil`, on Linux in `$XDG_STATE_HOME/veil` (or `~/.local/state/veil`).

**Update check** (desktop app, from 16/11/2026, `veil-app/src/update/`):
- Once per start the app asks `api.github.com` for Veil's latest release, over HTTPS. It
  sends the User-Agent `veil-app/<version>` and nothing about you; GitHub sees your IP
  address and that a copy of Veil started, as it would for any website.
- If a newer version is out, the home screen shows "Update available" with a link to the
  release page. The link is built from the version number alone, never taken from GitHub's
  answer.
- Untick "Check for updates" on the home screen and no request is made at all, now or on
  later starts.

**Installing an update** (desktop app, from 17/11/2026, `veil-app/src/update/apply.rs`):
- Only when you click "Install", and only for a copy installed from the Windows MSI, the
  macOS DMG or the Linux AppImage. Portable, deb, rpm and tar.gz copies keep the link.
- Veil downloads three files from the release on GitHub, each named from the version
  number: `SHA256SUMS.txt`, `SHA256SUMS.txt.sig`, and its own package.
- It installs nothing unless `SHA256SUMS.txt.sig` is a valid Ed25519 signature over
  `SHA256SUMS.txt` by Veil's release key, whose public half is built into the app
  (`crates/veil-app/release-key.pub`), and the package's SHA-256 matches its line in that
  list. A changed file, a changed list or another key is refused, and the download deleted.
  So someone who controls GitHub's answer, the network, or the release page cannot make
  Veil install their file; they would need the release key, which is kept only as a
  secret of the release workflow.
- The package names its version, so an older signed list cannot pass an older package off
  as a newer one.
- Then: Windows hands the MSI to Windows Installer, which asks for permission and replaces
  Veil once it has closed. macOS copies `Veil.app` out of the disk image over this one and
  reopens it. Linux renames the new AppImage over this one and starts it.
- The release key was made on 17/11/2026 and is built into 0.1.2 and later. Its public half
  is `b7979c9c23c5061f66fb8183de91a970ff9d981fb3a65797ca4914e9fd76f4a8`. Copies from 0.1.x
  have no key and only link to the release page.

**Signed installers** (from 17/11/2026, `packaging/`, `.github/workflows/release.yml`): the
Windows programs and MSI are to be Authenticode-signed through SignPath Foundation's free
open-source programme ([CODE_SIGNING.md](CODE_SIGNING.md)), and `Veil.app` signed with a
Developer ID and notarised by Apple, once those are in place. Neither is yet. Until then the
files are unsigned (Windows) or ad-hoc signed (macOS), and the workflow says so. Every
release file is listed in `SHA256SUMS.txt` with build provenance from GitHub.

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
  alike. Letters that only look the same, such as Latin "a" and Cyrillic "Ð°", are not caught.
  Who someone is rests on the safety number; the host badge comes from the invite's host
  key, not from a name.
- Chat message sizes are not padded, so a relay or forwarder sees roughly how long each
  message is.
- Video and sound prove only that a member of the call sent them, not which member. Every
  member holds the call's shared key. In a direct (mesh) call each member's link may carry
  only that member's video and sound, so this is closed there (fixed 17/11/2026). Through a
  forwarder, a member who changes their own copy of Veil could still send video or sound that
  shows in another member's tile, or stop that member's real video and sound from showing
  until the key next changes. Relays and forwarders cannot.
  A shared screen is different: each picture is signed with the sender's device key, so no
  member can show one as someone else's share. A member can still stop a real share's
  pictures from showing in the same way.
  Names and chat are different too: each is signed with the sender's device key, so no member
  can send one as someone else (fixed 16/11/2026), stop someone else's (fixed 17/11/2026), or
  replay one from an earlier call (fixed 17/11/2026).
- After the key changes, frames sealed under the previous key are still accepted for 2 s, so
  video and sound already in flight are not lost. When the change removed someone, that
  window is closed: the previous key stops working at once, because the removed member still
  holds it (fixed 16/11/2026). Members who stay may lose a moment of video or sound.
- Keyframe requests are not authenticated end to end. A forwarder or member can ask a sender
  for keyframes at will. That costs the sender bandwidth, but no more than one keyframe per
  500 ms, and it reveals nothing.

## 3. No telemetry, no default servers

Veil sends nothing to its authors and has no built-in servers. Every rendezvous, relay and
forwarder address comes from an invite, a config file or an environment variable you control
(`VEIL_RENDEZVOUS`, `VEIL_ADVERTISE`, `VEIL_PORT` for the desktop app). To find its LAN
address, the app asks the operating system which interface would route to a documentation
address (192.0.2.1); no packet is sent. It finds its public IPv6 address the same way, with
2001:db8::1. On Windows and macOS that is usually a temporary (privacy) address, which the
system replaces about once a day; Veil never uses a link-local or private (ULA) IPv6 address
in an invite. Addresses in the source
code are from the documentation ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24,
2001:db8::/32) and appear only in tests.

**Your firewall.** The Windows MSI adds the rule below for the installed `veil-app.exe` when
you install it (you confirm Windows' prompt for the install), and removes it when you
uninstall. On Windows, before you host, the app reads the firewall rules (PowerShell,
`Get-NetFirewallApplicationFilter`, read only) to see whether guests can reach it. If not, it
shows an "Allow guests" button. Only that button changes anything: it runs
`netsh advfirewall firewall add rule name="Veil"` through Windows' administrator prompt, which
you confirm or decline. The rule allows inbound UDP to this `veil-app.exe` only, on Private and
Public networks. Remove it in Windows Defender Firewall (Inbound Rules, "Veil") at any time.
The code is `crates/veil-app/src/firewall.rs`.

**Your router.** Off until you say yes: Veil asks once, before the first call you host, and
the choice is under Settings > Connection ("Open a port on my router"). When it is on and you
host a call, Veil asks your router to forward two UDP ports to this computer: the call's port,
and the port of the rendezvous Veil runs for the call, which guests use to reach each other.
With your own rendezvous set, only the call's port is forwarded. Both are for
that call only. It tries PCP, then NAT-PMP (both UDP 5351 to your default gateway), then
UPnP (an SSDP search on the local network, then plain HTTP to the router that answered). The
router learns this computer's LAN address and the ports, and each mapping is named "Veil". A
lease lasts 20 minutes, is renewed while the call runs, and is removed when it ends. A router
that allows only permanent UPnP mappings gets one, removed when the call ends; if Veil
crashes it stays until the router restarts or you remove the "Veil" entry. Veil talks only to
your default gateway, or for UPnP to the device that answered from that address, and follows
no redirect or proxy. With the port open, the router's public address goes first in your
invites, so anyone holding an invite learns it (section 2). To find the default gateway,
Veil reads `/proc/net/route` on Linux and runs `route print` on Windows or `route get` on
macOS. The code is `crates/veil-portmap` and `crates/veil-app/src/engine/router.rs`.

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
- Background blur uses Google's MediaPipe selfie segmenter (landscape), licensed Apache-2.0,
  from the MediaPipe model card, and credited in DEPENDENCIES.md under "Other components". Its weights are converted once by `tools/selfie_weights.py`,
  which checks the downloaded file's SHA-256, and are built into Veil as a 213 KB file. Veil
  never downloads a model at run time and never takes one from another member, so parsing a
  model is not attack surface. Source, hashes and how to regenerate are in
  `crates/veil-media/src/segment/MODEL.md`. Veil runs the network with its own safe Rust code,
  not an inference library: see [ADR 0001](adr/0001-on-device-segmentation-for-background-blur.md).
- Encryption, signing and hashing come from the RustCrypto crates (`chacha20poly1305`, `hkdf`,
  `hmac`, `sha2`, `ed25519-dalek`) and `hpke`. Known-answer tests
  (`crates/veil-security/tests/known_answers.rs`, `crates/veil-group/tests/crypto_known_answers.rs`)
  pin the exact bytes of frames, safety numbers, invite links and MLS crypto, so a library
  upgrade cannot silently change what goes on the wire.

## 5. Code Rust cannot check

**Today:** the workspace sets `unsafe_code = "forbid"`, so the compiler refuses `unsafe` in
every crate except `veil-media`. That crate sets `deny` instead, and only its `ffi` modules allow
it: one per operating system (Windows, macOS), plus libopus and OpenH264. The rest of
`veil-media` is safe Rust.

**Native code in dependencies:** `ring` (used by `rustls` and `quinn` for TLS) is written partly
in C and assembly. It is licensed Apache-2.0 and ISC. The desktop app (`veil-app`) draws with
`eframe`, `winit` and `wgpu`, which call the operating system's window and graphics APIs
(DirectX 12 or Vulkan on Windows) through their own `unsafe` code; Veil's own app code has none.
On Linux and macOS, `veil-media` reaches the microphone and speakers through `cpal` (ALSA,
Core Audio) and the Linux camera through `v4l` (Video4Linux2 ioctls); both crates hold their
own `unsafe` code, and Veil's code that uses them has none. MJPEG camera pictures on Linux are
decoded by `zune-jpeg`, which is pure Rust.

**Fonts and icons:** none are bundled. `veil-app` reads the system's own UI, symbol and icon
fonts at start-up (Segoe UI and Segoe Fluent Icons or Segoe MDL2 Assets on Windows). On Linux
it looks for DejaVu Sans, Noto Sans, Adwaita Sans or Liberation Sans, searching the standard
font folders by file name, and the deb and rpm packages depend on DejaVu Sans. Where
there is no icon font (Linux, macOS), icons are drawn from vector strokes in code. egui's
bundled fonts are turned off because their licences are outside the allow list. The one
bundled image is Veil's own app icon (`crates/veil-app/assets`), made for Veil and under
Veil's licence. On Windows, `build.rs` embeds it in the exe with `winresource`.

**In `veil-media`:**
- Camera capture is only available through each operating system's own APIs: Media Foundation
  (Windows), AVFoundation (macOS), Video4Linux2 (Linux, through the `v4l` crate). Hardware
  video encoding is used on Windows (Media Foundation). Calling them needs `unsafe`
  foreign-function calls. These libraries ship with the operating system; Veil does not
  bundle them.
- H.264 on Linux and macOS is Cisco's OpenH264 (BSD-2-Clause), compiled from source by the
  `openh264-sys2` crate. **Patents:** Cisco pays the H.264 patent pool's royalties only for its
  own prebuilt binary downloaded at install time, so a build from source has no such cover. This
  is a `shortcut:` for an early release. Before a wide release, move to VideoToolbox (macOS) and
  VA-API (Linux), or load Cisco's binary at install time. Windows uses Microsoft's encoder and
  decoder, which ship with Windows.
- Opus audio means the libopus C library (BSD-3-Clause). Its sources are in
  `crates/veil-media/vendor/opus` (version, origin, what was left out, and a file-by-file
  check against xiph.org's release tarball in `VENDORED.md`),
  and `build.rs` compiles them with the `cc` crate. The deep-learning features are not built.
- Rules for that code:
  - `unsafe` is allowed only in `veil-media`, and only inside one `ffi` module per platform,
    plus `ffi/opus.rs` for libopus and `ffi/openh264.rs` for OpenH264.
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
| `veil-media/src/ffi/windows/app_window.rs` | `GetWindowLongW` (extended style), `GetWindow` (owner), `FindWindowExW`, `GetWindowThreadProcessId`, `SetWindowDisplayAffinity` | Leaving overlays and helper windows out of the screen share picker, by reading those two properties of each window. Veil's sharing bar, and its main window while the picker is open, are found by their fixed titles, checked to belong to Veil, and kept out of screen captures |
| `veil-media/src/ffi/windows/screen_stream.rs` | Windows Graphics Capture (`GraphicsCaptureItem` through `IGraphicsCaptureItemInterop`, `Direct3D11CaptureFramePool`, `GraphicsCaptureSession`) and Direct3D 11 (`D3D11CreateDevice`, `CreateTexture2D`, `CopySubresourceRegion`, `Map`, `Unmap`) | The live picture of a shared screen or window, and of the thumbnails while the picker is open. Each picture is copied out within the row pitch Windows reports. Thumbnails stay on your device. Only after you press Share are pictures sealed and sent like camera video |
| `veil-media/src/ffi/windows/protect.rs` | DPAPI (`CryptProtectData`, `CryptUnprotectData` with `CRYPTPROTECT_UI_FORBIDDEN` and a per-purpose label as extra entropy, `LocalFree`) | Sealing `veil-app`'s saved device key and verified list to the signed-in Windows user. It sits in `veil-media` only because all of Veil's `unsafe` code does |
| `veil-media/src/ffi/opus.rs` (every platform) | libopus: `opus_encoder_create`, `opus_encode_float`, `opus_encoder_ctl`, `opus_decoder_create`, `opus_decode_float`, the matching `_destroy` calls, `opus_get_version_string` | Opus audio. Every buffer is a Rust slice whose length is the count passed to libopus, and packet lengths are checked before decoding. Each state is freed once, on drop |
| `veil-media/src/ffi/openh264.rs` (Linux and macOS; also built for its tests on Windows) | OpenH264's encoder and decoder through their vtables: `WelsCreateSVCEncoder`, `GetDefaultParams`, `InitializeExt`, `SetOption`, `ForceIntraFrame`, `EncodeFrame`, `WelsCreateDecoder`, `Initialize`, `DecodeFrameNoDelay`, `Uninitialize` and the matching `WelsDestroy*` calls | H.264 off Windows, one encoder per simulcast rung with temporal layers, and decoding other members' video. The bitstream comes from another member, so OpenH264's decoder is part of the attack surface. Veil refuses any decoded size above 4K, reads each output plane only within the rows and pitch OpenH264 reports, and copies it out before the next call. Each encoder and decoder is destroyed once, on drop |
| `veil-media/src/ffi/macos` (`camera.rs`) | AVFoundation (`AVCaptureDevice` listing, permission request, formats and frame rates, `AVCaptureSession`, `AVCaptureDeviceInput`, `AVCaptureVideoDataOutput` with a sample buffer delegate on a serial dispatch queue) and CoreVideo (`CVPixelBufferLockBaseAddress`, plane base addresses, pitches and heights) | Camera pictures on macOS, converted to NV12 by AVFoundation. Each picture is copied out of the locked pixel buffer within the plane sizes CoreVideo reports, then unlocked. macOS asks the person for camera permission; a refusal stops the camera. The camera's name stays on the device |
| `veil-media/src/ffi/macos/screen.rs` | Core Graphics (`CGPreflightScreenCaptureAccess`, `CGRequestScreenCaptureAccess`, `CGWindowListCopyWindowInfo`) and Core Foundation (`CFArrayGetCount`, `CFRelease`) | Checking the Screen Recording permission before capturing, and whether a shared window that is out of sight still exists. Only a yes or no is kept. No window's details are read |

## 6. Known shortcuts

Where the code takes a known shortcut, it says so in a comment that starts with `shortcut:`,
with the limit and when to fix it. To list them all:

```bash
grep -rn "shortcut:" crates
```

[HANDOFF.md](HANDOFF.md) keeps the same list in plain English.

## 7. Not yet tested

- Linux and macOS media on real devices. The camera, microphone and speaker code there is
  compiled and linted in CI, and OpenH264 encoding and decoding is round-trip tested, but no
  call has yet run on a Linux or macOS machine.
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
- The DMG and the AppImage on real machines, and the "Install" button against a real signed
  release. The MSI was installed, upgraded and uninstalled on Windows 11 on 17/11/2026.

**Known shortcut in updates:** on Windows the checked MSI waits in your own folder until
Windows Installer opens it, so a program already running as you could swap it in that
moment. Windows' prompt would then name an unknown publisher instead of Veil's (once the MSI
is signed). Such a program can already do anything you can.

## 8. Reporting a security problem

There is no private reporting channel yet. One will be set up before the first public release.
