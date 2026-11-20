# Phosphor icons (subset)

`PhosphorVeil.ttf` is a subset of the Phosphor icon font, regular weight, from the npm
package `@phosphor-icons/web@2.1.1` (`src/regular/Phosphor.ttf`). Licence: MIT, in `LICENSE`.

Veil draws its icons with it on macOS and Linux, where there is no system icon font. Windows
uses Segoe Fluent Icons or Segoe MDL2 Assets from the system instead.

It holds only the icons Veil uses (7.5 KB in place of 488 KB):

microphone, microphone-slash, video-camera, screencast, users, chat-centered-text,
phone-disconnect, lock, shield, squares-four, copy, x, wifi-high, paper-plane-right, link,
speaker-high, info, palette, hand-palm, smiley, key, folder, play, arrow-clockwise,
flip-horizontal.

## Adding an icon

1. Find its codepoint in the package's `src/regular/style.css` (`.ph.ph-<name>:before`).
2. Add the codepoint to `Icon::phosphor` in `src/icons.rs` and the name to the list above.
3. Rebuild the subset with fontTools, listing every codepoint in `Icon::phosphor`:

```
python -m fontTools.subset Phosphor.ttf --unicodes=U+E326,U+E328,... \
  --output-file=PhosphorVeil.ttf --no-hinting --drop-tables+=GSUB,GPOS,FFTM
```

The `icons` tests check that every icon's codepoint is in this file.
