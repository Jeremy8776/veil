# Changelog

All notable changes to Veil. Dates are dd/mm/yyyy.

## Unreleased

### Fixed

- Veil now opens on Linux systems that keep fonts in other folders, such as Fedora. It searches
  the standard font folders for a common font, and the deb and rpm packages install DejaVu Sans.
- `veil.log` now records the error when Veil cannot start, for example with no graphics
  driver, display or font. Before, the log stopped at the version line.

### Added

- You can turn off the mirrored view of your own video, before a call or from the video menu
  in a call. It changes only what you see: others always see you the normal way round.

## 0.1.8, 18/11/2026

### Security

- An approved joiner can no longer end a call for everyone with an oversized key package. The
  host refuses packages over 2,048 bytes and stays in its current epoch if a group message
  could not be sent. (GHSA-jjjr-ghc3-p32r)
- An approved joiner can no longer stop the host removing or admitting people by joining with
  an unusable key. The host now refuses keys it cannot encrypt to. (GHSA-34xp-qw4v-x3hv)
- Idle links can no longer fill a shared relay. Unused sessions can be taken back after 30
  seconds, one address can hold at most 128, and a link that joins no room is closed after 60
  seconds. (GHSA-qc6p-pgpg-wm5g)
- Someone holding an invite can no longer use up a host's relay places. A relay session that
  carries no link gives its place back after 10 seconds. (GHSA-84gr-272c-4c7r)
- A device now dials back only for an introduction it has just agreed to, within fixed limits,
  so an invite holder or a rendezvous cannot use it to send packets to chosen addresses.
  (GHSA-6r94-c7q8-rq36)

### Changed

- Invite links are now short, about 45 characters instead of a few hundred. The link holds
  where to fetch the invite and the key that opens it, and the call's rendezvous keeps the
  invite encrypted until the call ends. Guests need this version to use a short link. "Copy
  full link" gives the long form, which also works for older versions. Veil shows the long
  form by itself when the rendezvous cannot hold the invite or cannot be reached from as far
  as the call can.

### Fixed

- Guests on the internet can now reach each other, not only the host. With "Open a port on my
  router" on, a host without a rendezvous of its own choosing now opens a second UDP port for
  the call, for the rendezvous Veil runs. This is a change from the one port Veil opened
  before. People warns if the second port does not open. Everyone in the call needs this
  version for guests to connect to each other.

## 0.1.7, 18/11/2026

### Fixed

- Updating from the app now opens Veil again when it is done, on Windows, macOS and Linux.
  Veil gets the new version ready while it is open, so a problem shows in the app, then
  closes, and a helper swaps in the new version once Veil has closed. If the swap fails, the
  old version opens instead and the log says why.
- "Update available" opens the release page on macOS and Linux too. A Veil run straight from
  the disk image, or from Downloads, links to the release page instead of failing to update.
- The microphone, speaker, camera and Effects menus in a call work again. Choosing from them
  used to close the menu without changing anything.
- The lock in the call's top bar showed as an empty box. It now shows the lock icon.

### Changed

- The call menus are now tick lists: one click picks a device. Volume is a slim slider, blur
  is an Off / Light / Strong switch, and filters are swatches.
- Mute and Stop video are joined to their menu arrows as one split button.
- The call's top bar groups the name, time and an Encrypted badge on the left, and the
  people count, Invite, stats and view on the right. The count opens People.
- Background blur's outline follows the person more closely: hair, fingers and shoulders keep
  their shape instead of looking blocky, and the edge no longer flickers when you sit still. It
  still keeps up when you move. Nothing new is downloaded and the app is no bigger.

## 0.1.6, 17/11/2026

### Added

- Change devices during a call. The small arrow beside Mute picks another microphone or
  speaker and sets the microphone volume (up to 200%) and the speaker volume. The arrow
  beside Stop video picks another camera. The call carries on without leaving, and a
  headset plugged in during the call shows up the next time you open the menu.
- Call stats, for finding out why a call sounds or looks bad. The signal button in the top
  bar (or Ctrl+Shift+D) shows each person's connection path, round-trip time, packet loss,
  bit rate down and up, the size and frame rate of their video, and how loud their sound
  arrives, with your own microphone level. Off by default. The numbers stay on your device.

### Changed

- The log records each time a microphone, camera or speaker opens or fails to open, and
  whether it was the one you chose or the system default. It names no devices. This helps
  find out why a chosen microphone might not be used.

## 0.1.5, 17/11/2026

### Added

- The host sees how long the invite link still works ("Expires in 42 min") on the invite
  card, in People and on the Invite button. A link that runs out unused is replaced. Tick
  "Keep links working until the call ends" to make links that last the whole call. Each link
  still lets one person in, and the host still admits them.

### Changed

- The home screen no longer shows a "Veil" title under the logo, and its content sits in the
  middle of the window. The call-ended screen and Settings > Connection do the same.
- The host's log records when each guest joins and leaves, and when linking up with someone
  takes over a second. This helps find calls where a leave goes unnoticed.

### Fixed

- Guests on the internet can hear, see and chat with a host whose router port is open. In
  about half of calls the host tried to reach the guest instead, which fails when the guest's
  router has no open port: the guest joined but no sound, video, name or chat came through.
  Now the guest connects to the host. Both need this version.

## 0.1.4, 17/11/2026

### Added

- Video filters: None, Warm, Cool, Black & white and Bright. Pick one before you join, or
  from the new Effects button in the call bar. It changes your self-view and the picture other
  members receive, on your device, before the picture is encoded. It resets to None each time
  Veil starts.
- Background blur: Off, Light or Strong, next to Filter. It finds you in the picture on your
  device, with a small network built into Veil, and blurs everything else before the picture is
  encoded. Nothing is downloaded or sent to do it. If your computer is too busy, your camera
  turns off and Veil says so, so your background is never sent sharp. Blur is not a privacy
  guarantee: edges and hair can show what is behind you. It resets to Off each time Veil
  starts.
- Open a port on your router, so guests on the internet can reach a call you host. Off until
  you allow it: Veil asks once before your first hosted call, and the choice is in Settings >
  Connection. Veil asks the router (PCP, NAT-PMP or UPnP) for the call's one UDP port, renews
  it while the call runs and removes it when the call ends. People shows whether it worked.
  If the router says no, the tooltip says how to open the port by hand. If your provider puts
  you behind a shared address (carrier-grade NAT), it says a public veil-node is needed.
- A check that guests can reach you. With a public veil-node set, Veil asks it to try your
  call port from outside, once the router opens it or when you set a public address by hand.
  People shows "Reachable from the internet" or "Not reachable from the internet". The node
  must run 0.1.4 or later.
- Settings > Connection ("Connection" on the home screen): choose your own rendezvous and
  forwarder, your public addresses and a fixed call port in the app. Veil saves them on this
  device, and only when every field is valid. A `VEIL_*` environment variable still wins,
  and the screen says so.
- Servers by name: a rendezvous or forwarder can be `KEY@HOST:PORT` with a DNS name, in
  Settings and in `VEIL_RENDEZVOUS` and `VEIL_FORWARDER`. Veil looks the name up when the
  call starts. The server's key is still checked, so a wrong answer can stop a call but
  cannot listen in.

### Changed

- A new white icon: the same rings and V, without the teal square, and with the V cut out.
- The home screen shows the logo with slow waves, and the smaller links (Demo call,
  Connection, device key, Logs) sit in one row under the card, with the version below.

## 0.1.3, 17/11/2026

### Added

- Log files to send when something goes wrong: "Logs" on the home screen opens the folder.
  `veil.log` is this run, `veil.previous.log` the run before (the one to send after a freeze
  or crash). No names, chat or invite links: see TRANSPARENCY.md.

### Fixed

- The window no longer freezes (on Windows, "Not Responding") when a call ends while a
  camera, microphone or speaker driver is stuck. Veil waits at most a second for each device, then
  carries on.

## 0.1.2, 17/11/2026

### Added

- Installers: an MSI for Windows (adds the firewall rule guests need), a DMG with a signed,
  notarised `Veil.app` for macOS on Apple Silicon and Intel, and an AppImage, deb and rpm for
  Linux. Signed where the release has certificates.
- Updates: copies installed from the MSI, the DMG or the AppImage can install a newer release
  with one click. Veil checks the release's Ed25519 signature and each file's SHA-256 first.
  0.1.2 is the first release that carries the release key, so copies from 0.1.x update by
  hand once, to 0.1.2.

### Fixed

- Windows: the taskbar, title bar and Explorer show Veil's icon. The exe and the window had
  none, so Windows showed its default.

## 0.1.1, 17/11/2026

Security fixes. Update every device in a call: see the upgrade note below.

### Security

- One sealed packet from a member can no longer block another member's names and chat, on any
  route, or their video and sound on a direct link. Frames on a direct link must come from the
  member they claim, and the names and chat window moves only after the device signature
  checks out. (GHSA-rvhv-r5mr-2w3h)
- A name or chat line signed in an earlier call can no longer be replayed into the current call.
  The signature now covers a value unique to the call and epoch. (GHSA-qqx7-3x2m-v277)
- One forged audio packet with a far-ahead sequence number no longer silences a member at a
  receiver. (GHSA-9wq4-7r8v-7582)
- A member that withholds QUIC stream credit can no longer stall another participant's call
  loop, or block removal on the host. No media send waits on one peer without a deadline.
  (GHSA-37f5-22p2-q6hr)
- `veil-node` forwarder: queued video is bounded per node and per source, and stuck frames
  expire. One device key can no longer use up the node's memory. (GHSA-vrpc-82hc-76gf)

Still open: through a forwarder, a member can block another member's video and sound until the
next epoch. See docs/TRANSPARENCY.md.

**Upgrade note:** signed names and chat use message format version 3. Devices on 0.1.1 and
devices on 0.1.0 ignore each other's names and chat.

### Fixes

- Guest join shows "Reaching the host" until the knock lands, and Leave works at once.
- Icons are drawn in code where the system has no icon font (Linux).
- Microphone and speakers survive audio glitches (xruns), device reroutes and a refused
  real-time priority instead of stopping for the rest of the call.

### Linux and macOS

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
