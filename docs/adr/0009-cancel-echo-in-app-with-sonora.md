# Veil cancels echo itself, with a pure Rust port of WebRTC's canceller, on by default

Status: accepted (2026-11-26)

Echo is other people's voices coming back to them because this device's microphone picked up
what Veil played. Until now Veil had no echo cancellation of its own: Windows applied the
driver's voice effects where it had them, and Linux and macOS had nothing. People on speakers
or a leaky headset sent everyone's voice back to them.

Veil now cancels echo in the microphone thread, first in the chain, using the final speaker mix
as the reference, with `sonora` (a pure Rust port of WebRTC's AEC3, BSD-3-Clause). It is on by
default, with a "Cancel echo" switch. On Windows, while the switch is on, the microphone opens
without the driver's voice effects (raw mode, where the driver supports it), so echo is not
cancelled twice; switched off, it opens with them as before.

## Considered options

- **The desktop's canceller** (PipeWire `module-echo-cancel`, macOS voice processing, Windows
  driver effects). Rejected: off by default on Linux, needs setup people will not do, and
  behaves differently per platform and device. The person with the echo rarely knows they have it.
- **`webrtc-audio-processing` (C++ through tonarino's crate).** Rejected: the bundled build needs
  meson, ninja and abseil and does not support Windows; the system library (1.x/2.x) is missing
  from Debian bookworm and Ubuntu 22.04/24.04 and cannot be used from the AppImage.
- **Speex's canceller** (`aec-rs`, `speexdsp`). Rejected: older algorithm, weaker results.
- **`aec3` crate.** Rejected for now: its API is still changing.
- **`sonora`, in app, all platforms** (chosen). Plain `cargo` build everywhere, checked against
  the C++ test suite. Measured in Veil: 0.5–1.1% of one core at 48 kHz mono (Ryzen 5600X;
  the higher figure during a live call), and +0.6 MB.

## On by default

Veil's rule is that the base stays light and heavier features are opt-in. Cancel echo is on by
default anyway: its cost is about 1% of one core and no download, and echo harms
the other people on the call, not the person causing it, so an opt-in would rarely be turned on.

## Consequences

- A young dependency (first released 2026): pin the version and keep the synthetic echo test as
  the guard when updating.
- Only Veil's own playback is cancelled; sound from other apps still reaches the microphone.
- Windows loses the driver's noise suppression on the microphone; Veil's own noise removal covers it.
- Two copies of Veil on one computer still need headphones: neither sees the other's playback.
