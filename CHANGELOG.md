# Changelog

All notable changes to Veil. Dates are dd/mm/yyyy.

## Unreleased

### Added

- **Emoji in chat.** Emoji now show in colour in messages, from the system's own emoji font:
  Segoe UI Emoji on Windows, Apple Color Emoji on macOS, and Noto Color Emoji (or Twemoji)
  on Linux. The smiley button beside the message box opens a picker.
- **Reactions.** Click a message to react with 👍 ❤ 😂 😮 😢 or 🎉. Reactions show under the
  message with a count; hover to see who reacted, and click one to add or take back yours.
  Reactions are sealed and signed like chat. Messages from Veil 0.1.11 and earlier show as
  before but take no reactions, and 0.1.11 cannot read chat from this version.

- **Lobby alerts** for the host. When someone starts waiting in your lobby while Veil is in
  the background, you get a notification ("Ana wants to join") and Veil asks for attention:
  the taskbar button flashes on Windows, the Dock icon bounces on macOS, and the window is
  marked urgent on Linux. Clicking the notification brings Veil to the front. Both clear
  when you look at Veil or nobody is waiting.

- **Check everyone at once.** With two or more others in a call, the Security panel offers a
  group code. Each person posts theirs in a group chat you all trust, then pastes what everyone
  posted. Each matching code verifies the person who posted it. Codes from before someone
  joined or left are reported as old, not as a mismatch.

- **Noise suppression.** Veil now removes background sound (fans, typing, traffic, hum) from
  your microphone, using a small speech model (RNNoise) that runs on your computer. It is on
  by default; turn it off under Audio settings > Noise suppression, for example to play music.
  **Remove noise from others**, under Speaker in the same menu, cleans everyone else's sound
  too. It is off by default and uses a little more processor for each person. The model adds
  10 ms of delay.

### Changed

- The pre-join card is grouped into Video, Audio and Connection, and scrolls in a short
  window. Start and Back always stay in view.
- Veil remembers your camera, microphone, speaker, filter, background blur, mirror and both
  volumes between runs, whether you chose them before or during a call. A saved device that
  is unplugged is kept for next time, and the default is used meanwhile.
- A newer Veil now shows as a card at the top of Home, with Install and restart (or Get the
  update where Veil cannot install itself). An invite link that does not open says it may
  need a newer Veil while an update is waiting.

- `veil.log` now notes when a link to someone drops and when it comes back, and the host's
  log notes each joiner it turns away, with the reason.

- "Copy my code" now copies a short message that says who it is from and for, then the code.
  Paste the whole message: Veil finds the code inside it, and it still compares all 60 digits.

### Fixed

- A raised hand now shows for everyone in the call, not only on your own screen. Lowering it
  clears it for the others, and someone who joins later sees hands that are already up. The
  hand is sealed and signed like names and chat. Older versions of Veil do not show it.
- Sharing a selected area of your screen now shows live to everyone watching, and changing
  the area during a share keeps it live. Before, a new shared size restarted the picture
  numbering, so those watching kept seeing the last picture from before the change. The same
  fix covers a camera that changes size and a change of Data use mid-call.

- A member whose connection dropped for a moment (for example Wi-Fi off and on) no longer
  stays on "Connecting" until someone joins or leaves. Veil now links up again by itself,
  trying again after 1, 2, 4, 8 and then every 16 seconds.
- Re-opening an invite link that was already used now says so, and asks you to get a new one
  from the host. It used to say "The host did not let you in."

## 0.1.11, 19/11/2026

### Added

- **Data use**, on the pre-join screen and in the camera menu during a call. Auto
  (recommended) gives the best picture the connection allows. On a slow link it uses Data
  saver's smaller picture and returns to full once the link has been steady for a while; the
  camera menu shows "Auto: saving data" meanwhile. Data saver sends one small picture at about 150 kbit/s and
  sound at 16 kbit/s. Audio only turns your camera off and sends sound at 16 kbit/s. Through a
  forwarder, both also ask for others' smallest picture. The choice is kept between calls.
- **Hide my address**, in Settings > Connection. When you join a call, Veil reaches everyone
  through the call's relay only, so the others never learn your IP address. Off by default.

## 0.1.10, 19/11/2026

### Fixed

- Veil's taskbar icon no longer goes blank after an update on Windows. The Start menu shortcut
  now takes its icon from `veil-app.exe`, not from a copy that each update deleted. A pinned
  Veil that already lost its icon needs unpinning and pinning again once.

### Security

- A device can no longer make a node or app hold hundreds of megabytes per link by sending data
  that Veil never reads. Each link now holds at most about 5 MB of unread data
  (GHSA-g7j3-q88p-63cf).
- One party can no longer take every room on a shared forwarder by holding many empty rooms
  open. Each address can hold 16 rooms at most, a room whose only member stays alone for 60
  seconds is closed, and the default room limit is now 4,096 (GHSA-rgc8-82r3-49vh).

## 0.1.9, 18/11/2026

### Fixed

- Veil now opens on Linux systems that keep fonts in other folders, such as Fedora. It searches
  the standard font folders for a common font, and the deb and rpm packages install DejaVu Sans.
- `veil.log` now records the error when Veil cannot start, for example with no graphics
  driver, display or font. Before, the log stopped at the version line.
- A guest who is still linking up now sees at once that the host ended the call or removed
  them, and can leave at once. Before, this could take about 11 seconds.
- The Start a call and Join a call screens now sit in the middle of the window, and the
  preview grows with it. Before, they stayed in the top left corner of a large window.

### Security

- One sender can no longer use up a relay's whole capacity and starve other calls. When a relay
  runs short, each address gets an equal part, and spare capacity stays available to anyone.

### Added

- Share your screen during a call, next to your camera. Pick a screen or an application from
  live thumbnails, and optionally select an area of it. A shared window or area keeps going
  when you switch to another app. You see what you are sending, and a small bar over your
  other apps lets you change it or stop. The picture is sealed end to end like your camera, and
  only the sealed header says it is a screen. Each picture is signed by the sender's device, so
  nobody in the call can pass off a picture as your share. The picker stays out of what you
  share. Everyone needs this version to see a share.
- Screen sharing has been tried on Windows only so far. On macOS Veil asks for the Screen
  Recording permission. On Linux it needs an X11 session and is turned off under Wayland, where
  each picture would be written to a file. The deb and rpm packages now need EGL and GBM.
- You can mirror your video, before a call or from Effects in a call. It flips your
  picture left to right for everyone in the call. It is off by default.
- Calls use IPv6 when both ends have it, and prefer it. Most UK broadband now gives every
  home an IPv6 address, and IPv6 has no NAT, so two people with IPv6 can usually link
  directly even when their IPv4 is behind carrier-grade NAT. The host's public IPv6 address
  goes in every invite, members tell the rendezvous theirs, and every dial tries IPv6 first
  and IPv4 a quarter of a second later.
- With "Open a port on my router" on, the host also asks the router (PCP) to let the call's
  port through its IPv6 firewall, and removes it when the call ends.
- With a public node that has an IPv6 address, the reachability check in People covers the
  host's IPv6 address too. People says "Reachable from the internet" if either one is.
- Call stats show "IPv6" beside the path of a link that runs over IPv6.
- People on mobile data connect sooner. Veil no longer waits for each way of connecting to
  fail before trying the next: the relay is tried after 5 seconds at most, instead of about
  half a minute. Calls also start with smaller packets, which more mobile networks carry.

### Compatibility

- Older versions still join calls from this version, and the other way round. Invites keep
  their format; an older version skips the IPv6 address it cannot dial and uses IPv4. Only
  calls where both ends run this version link over IPv6.

### Changed

- Your own video now shows exactly what others see. It used to be mirrored on your screen only.
- Verifying someone no longer means reading 60 digits. In the Security panel, swap codes through
  another app and paste theirs: Veil compares the whole number. Older versions can still
  compare by hand.
- Speaker view has its own icon, and the arrows beside Mute and Stop video are centred.
- Veil for Linux now needs glibc 2.39 or later: Ubuntu 24.04, Debian 13, Fedora 40 or newer.
  Ubuntu 22.04 and Debian 12 can no longer run it, because screen capture needs a newer
  PipeWire than they have.

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
