# Wayland sharing uses KWin on KDE and the portal elsewhere

Status: accepted (2026-11-20)

On Wayland an app may not list or capture other apps' windows through the display protocol, so
`xcap`'s screenshots do not work there (its fallback has the desktop write each picture to a
file). Veil shares on Wayland in one of two ways (`veil-media/src/screen/`):

- **KDE Plasma**: KWin's own interfaces. `org_kde_plasma_window_management` lists windows,
  D-Bus `org.kde.KWin.ScreenShot2` gives thumbnails and the area preview through a pipe, and
  `zkde_screencast_unstable_v1` streams the share over PipeWire. Veil's normal picker works.
- **Everywhere else**: the ScreenCast portal. The desktop's own chooser picks one source; Veil
  goes straight to Select area with that stream.

## Considered options

- **Portal everywhere.** One path, every desktop. Rejected as the only path: the person picks in
  the desktop's dialog, so Veil's picker (tabs, live thumbnails) cannot be used, and on KDE it
  could be.
- **`ext-image-copy-capture-v1` with `ext-foreign-toplevel-list-v1`.** Cross-desktop and
  dialog-free, but KWin 6.8 and GNOME do not offer them (wlroots desktops do). Revisit when they
  do; it would replace the KDE path.
- **xcap's portal recorder.** Rejected: it never ends its PipeWire thread or closes the portal
  session.
- **KWin on KDE, portal elsewhere** (chosen).

## Consequences

- Two capture paths to keep working on Linux Wayland. Both reuse one PipeWire stream module.
- On KDE, Veil can see and capture windows without the desktop asking, as on Windows and X11
  (TRANSPARENCY). KWin 6.8 allows this for any unsandboxed app; Plasma 6.4 and older need the
  `X-KDE-…` lines in Veil's `.desktop` file, so a build run without its installed `.desktop`
  falls back to the portal there.
- A Flatpak build is sandboxed and always uses the portal.
- `zkde_screencast_unstable_v1` and `org_kde_plasma_window_management` are KDE-specific and the
  first is still "unstable"; if KWin changes them, the portal is the fallback.
- Planned share sound ([ADR 0007](0007-share-sound-is-a-separate-opt-in-stream.md)) reuses the
  same PipeWire connection on both paths. This decision about pictures is unchanged.
