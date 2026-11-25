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

## Implementation notes (PR #92)

The desktop part follows this decision, with these differences and details:

- **Linux capture.** Veil opens its own capture stream on the session's PipeWire with
  `node.autoconnect` off, and links each app's playback stream (`Stream/Output/Audio`) to it,
  except streams of Veil's own process. This leaves Veil out more reliably than filtering a
  sink's monitor, which already holds the mix. The portal's PipeWire connection
  ([ADR 0005](0005-wayland-share-kwin-on-kde-portal-elsewhere.md)) cannot be reused: the
  portal's remote exposes only the screen's node, not the apps' playback streams. Outside a
  sandbox this needs no extra permission. A Flatpak build would need PipeWire access.
- **Changing what you share.** "Include computer sound" is off whenever the picker opens for a
  new share. Using the picker's Switch during a share keeps that share's choice, because it is
  the same share. It is never kept from one share to the next.
- **Wire mark.** The top bit of the sealed audio header's first byte marks share sound. Older
  versions read it as an unknown version and drop the packet.
- **Signature.** Each share sound packet carries an Ed25519 signature of the sender's device
  between the audio header and the Opus packet, bound to the epoch and frame id
  (`engine/sound_proof.rs`). Receivers also play share sound only while that sender's signed
  screen share is showing.
- **Data use.** Share sound is stereo at 64 kbit/s on Auto and 32 kbit/s in Data saver and
  Audio only.
- **Playback.** Receivers hear share sound in mono, because the call's speaker is opened in mono.
  This is a noted shortcut.
- **Provider grant.** The headless provider ([ADR 0006](0006-shared-engine-provider-contract.md))
  does not exist yet. When it does, share sound needs its own grant, as stated above.

## Implementation notes (SHARE-02, Android)

The Android part follows this decision, with these details:

- **Consent is the chooser.** Android's MediaProjection prompt takes the place of the picker,
  as the portal does on Wayland (`uses_system_chooser`). It asks every time and offers the whole
  screen or one app where the Android version does. Veil shows a short sheet first, with the
  sound choice and what Android will not capture, and skips area selection on the phone.
- **Java owns the projection.** `ScreenShare.java` holds the MediaProjection, virtual display,
  image reader and `AudioRecord`, keyed by a generation for each consent. Rust polls copies by
  generation; a stale generation gets nothing. The projection stops when the chooser's answer,
  the capture and the sound have all let go (`veil-media/src/projection.rs`), or when Android
  stops it.
- **Stop routes.** Stop share in the call bar, Stop sharing in the `mediaProjection` service's
  notification, Android's own indicator, revoked consent, the screen turning off, leaving the
  call and task removal all release the display, reader, sound and service together.
- **Phone sound.** Playback capture matches the media, game and unknown usages and excludes
  Veil's own app. Call audio uses the voice-communication usage, which is never matched, so
  there is no echo. It needs the microphone permission; without it the choice is greyed out
  and says why. Level, mute, stream, signature and Data use are SHARE-01's.
- **Size.** The virtual display is at most 1,280 pixels on its longest side and keeps the size
  it starts with. Turning the phone, or sharing one app of another shape, letterboxes the
  picture. This is a noted shortcut.
