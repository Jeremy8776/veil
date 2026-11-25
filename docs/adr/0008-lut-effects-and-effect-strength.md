# LUT effects and a strength for every effect

Status: proposed (2026-11-25). Covers roadmap FX-01. Not implemented yet.

Veil has four built-in Filters (Warm, Cool, Black & white, Bright) and Background blur. Each
is on or off, with no strength (`veil-media/src/effects.rs`, `veil-media/src/blur/`). People
want their own colour looks, as LUT files, and control over how strong any effect is.

## Decision

- **Standard `.cube` 3D LUTs.** A person imports a file on desktop or Android. Veil copies it
  into app-private storage and lists it by name next to the built-in Filters. It can be removed.
  The original file is not read again.
- **The file is untrusted input.** Parsing caps the file size and the grid size, accepts only
  the `.cube` keywords Veil uses, and rejects values that are missing, out of range or not
  numbers. A bad file gives a plain error and is not stored. The camera never stops because of
  a LUT.
- **Pre-computed for NV12.** Capture is NV12, but `.cube` LUTs map RGB. At import, Veil builds a
  bounded YUV-domain table, so per-frame cost is one interpolated table read per pixel. The cost
  must be measured at 1080p30 on desktop and on a phone before FX-01 ships.
- **One strength for every effect.** A 0 to 100% slider applies to built-in Filters, LUTs and
  Background blur. Strength blends the effect with the original picture: 0% leaves the picture
  untouched, 100% is the full effect. The default is 100%, so existing looks do not change.
- **Sender side, as today.** Effects apply before the self-view and before encoding, so everyone
  sees the same picture. They are not applied to screen shares. The choice and strength are
  remembered in preferences with the other effect settings.

## Considered options

- **Apply LUTs on receivers.** Rejected: receivers would see different pictures, and it would
  need a protocol change.
- **Convert each frame to RGB and back.** Simpler code, but it costs two colour conversions per
  pixel. Rejected unless measurement shows the YUV table is not accurate enough.
- **Also accept `.3dl` and other formats.** Not now: `.cube` is the common format, and each new
  parser is more untrusted-input surface.

## Consequences

- A new trust boundary: imported files. Parser tests must cover malformed, oversized and hostile
  input.
- Stored LUTs are local data. They are not sent, logged or included in Connection reports.
- The Glossary gains **LUT** and **Strength**. TRANSPARENCY's effects section must mention
  imported LUTs when FX-01 ships.
