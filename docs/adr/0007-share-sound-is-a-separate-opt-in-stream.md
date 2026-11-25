# Share sound is a separate, opt-in stream

Status: proposed (2026-11-25). Covers roadmap SHARE-01 and SHARE-02. Not implemented yet.

Screen sharing sends pictures only today ([ARCHITECTURE 3.5](../ARCHITECTURE.md#35-screen-sharing-today)).
People want to share a video or a demo with its sound, and to share from a phone. This
records how the sound and the phone share should work before the code is written.

## Decision

- **Opt in for each share.** The share picker has an **Include computer sound** choice. It is
  off at the start of every share and is not remembered. Choosing it is the consent for system
  sound capture. It is separate from microphone consent.
- **A stream of its own.** Share sound is a new Opus audio stream, marked as share sound inside
  the sealed header in the same way that a screen is marked as a screen. It is never mixed into
  the microphone. Receivers can then play, mute and measure it apart, and a forwarder sees one
  more stream without learning what it is.
- **Signed like the screen.** Share sound frames carry the sender's device signature, as
  screen pictures do, and follow the screen's lifetime. Ending the share ends the sound.
- **No echo.** Capture leaves out Veil's own call playback, so people do not hear themselves
  repeated back:
  - Windows: WASAPI process loopback that excludes Veil's process tree.
  - macOS: ScreenCaptureKit audio with Veil's own app excluded (macOS 13 and later).
  - Linux: a PipeWire capture of the default output sink, excluding Veil's playback stream
    nodes. Where the portal is used ([ADR 0005](0005-wayland-share-kwin-on-kde-portal-elsewhere.md)),
    sound uses the same PipeWire connection.
  - Where none of these is available, the picker says computer sound is not available there.
- **Two separate controls.** The sharer has a level slider and mute, applied before encoding.
  Each receiver has a local volume and mute for that share's sound, applied on playback only.
  Neither control changes the other person's setting.
- **Phone sharing uses the same path.** Android uses MediaProjection, with the system consent
  prompt every time and a foreground service of type `mediaProjection`. Phone sound uses
  `AudioPlaybackCapture`. Apps that block capture stay silent, and secure windows stay blank.
  Encoding, sealing and the wire format are shared with desktop; there is no Android-only
  protocol.

## Considered options

- **Mix share sound into the microphone.** Simpler, with no new stream. Rejected: receivers
  could not mute one without the other, noise suppression would damage music, and the sharer's
  mute would be ambiguous.
- **Remember the sound choice.** Rejected: system sound can include private notifications and
  other calls. A fresh choice for each share keeps consent explicit.
- **Capture all output, including Veil's.** Rejected: receivers would hear the call echoed back.

## Consequences

- One more audio stream per sharer. It counts towards Data use and forwarder capacity, and
  Data saver and Audio only need a rule for it (DATA-01).
- Three desktop capture back ends plus Android, each with its own permission and failure states,
  which the share UI must show plainly.
- TRANSPARENCY must describe system sound capture when it ships, including that it can pick up
  notification sounds.
- The headless provider ([ADR 0006](0006-shared-engine-provider-contract.md)) treats share
  sound as its own capture source, with its own grant.
