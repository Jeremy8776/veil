# Getting started

For Local only calls in 0.1.13, enable **Local only** in **Connection**, save, then
restart Veil before an offline session. Saved internet server fields and router mappings
are ignored in this mode. Use **Copy full link** or **Show full invite QR**; decoding a QR currently
requires another QR reader and pasting the resulting link. Nearby scanning is explicit;
hosts must opt into making their calls discoverable, and a guest types the nearby code
shown on the host's screen to get the invite. Nearby discovery currently uses IPv4
multicast. Full invitations remain available if multicast is blocked. Wi-Fi client
isolation can prevent all local connections even when devices share a network name.

The Local only boundary follows directly connected subnets reported by the operating
system, excluding recognised virtual/overlay adapters by name. Unrecognised or renamed
adapters and bridges carrying the LAN can still be included; it does not prove physical
proximity. No internet service is required for admission or media in this mode.


Veil makes private desktop calls without accounts. Get the same version on every device
from [Releases](https://github.com/Jeremy8776/veil/releases/latest), `veil-node` included.
Android phones install `veil-<version>-android-arm64.apk` from the same page.

## Make your first call

1. Open Veil, enter your display name and start a call. Check your camera, microphone and
   speaker on the pre-join screen. Headphones help prevent echo.
2. If you host on Windows and see **Allow guests**, use it to add Veil's inbound UDP rule.
   The MSI installer already adds this rule. Windows asks for administrator permission.
3. For calls outside your network, Veil can ask to open temporary ports on your router.
   Allow this if you want it to try; the choice is also in **Connection** on Home. If your
   provider shares your public address, use a public node chosen by you instead; see
   [Self-hosting](SELF_HOSTING.md).
4. Start the call and copy its invite from People. Send it privately to one guest.
5. The guest pastes the link into Veil, checks their devices and joins. Admit them from
   your lobby. Each invite is single use: copy a fresh link for the next guest.

The host decides who joins, even when a device is already marked verified. Leaving as the
host ends the call for everyone. **Demo call** on Home shows simulated people without a
network call; it does not verify that another device can reach you.

## Save and check connection setups

The **Your setup** summary on Home shows the active connection setup, call
scope, joining privacy, server status and whether incoming hosting has been verified.
Hover or focus a status for its explanation. Use the selector to switch setups; the
selected setup applies to your next call.

Open **Manage connections** to name the current setup or create one for another network
or VPS. **New setup** copies the current fields. **Save changes** validates the choices
and makes that setup active. Loading another setup in Connection only loads a draft until
you save it. Discard edits before loading another draft. Only inactive setups can be removed.
Existing settings become **Default**. Setup persistence has the same platform limitations
as other Connection settings; see [Transparency](TRANSPARENCY.md).

**Test these settings** checks the draft without saving it: it tries the chosen UDP port,
looks for local addresses, and verifies connections to configured servers. A reachable
server does not prove permission to host or that guests can reach you. Use a second device
to test an incoming call. Home's server result lasts five minutes in the current session
and applies only to the checked effective settings. Saved Local only remains a restriction
until a different setup or mode is saved; internet tests stay disabled until then.

## Sound, video and screen sharing

- The arrows beside the microphone and camera controls open their settings. Windows remembers
  device choices, effects and volumes between runs. Standard Linux/macOS runs currently keep
  these choices only while Veil stays open. Muting or stopping the camera closes that device.
- Noise suppression on your microphone is on by default. Turn it off in Audio settings
  when you want to transmit music or other background sound. **Remove noise from others**
  is optional and off by default. Both run on your device.
- Choose **Share**, pick a screen or application, optionally select an area, then press
  **Share**. A preview and a small sharing bar show what is being sent. Stop sharing from
  that bar or the call controls. Background blur applies to your camera, not your screen.
  A share sends pictures only for now; computer sound is planned.
- KDE Plasma Wayland uses Veil's picker where KWin permits it. Other Wayland desktops use
  the system chooser first. macOS requires Screen Recording permission. A cancelled choice
  sends nothing.
- **Mirror my camera** flips your camera for you and everyone in the call; screen shares
  keep their normal orientation.
  Windows and Android remember this choice across restarts. On Android, find it before
  joining or in the call's More menu. Blur is an effect, not a guarantee
  that your background cannot be recognised. See [Transparency](TRANSPARENCY.md).

## Check the people in your call

Open Security. A display name alone does not prove who someone is.

For a pair, copy your code and exchange it through an app or channel you already trust,
then paste their whole message into Veil. It compares all 60 digits. You can also compare
the number by hand. Do not use this call's own chat to establish trust.

With two or more other people, each person can post their group code in a trusted external
group chat. Paste the others' messages into Veil. A matching code verifies only the person
who posted it; everyone must provide their own. Old codes need replacing after membership
changes. If a short tag is ambiguous, check those people's pair codes instead.

Windows saves your device key and verified contacts using DPAPI. Linux and macOS currently
keep them only until Veil closes, so you need to check again after restarting there.

## Slow or costly connections

| Data use | What changes |
|---|---|
| Auto (recommended) | Uses the normal camera ladder, switches to Data saver on a strained link and returns when it recovers |
| Data saver | Sends a 320-wide camera picture at about 150 kbit/s and sound at 16 kbit/s |
| Audio only | Stops your camera and sends sound at 16 kbit/s |

Choose before joining or from the camera menu during a call. Audio only still receives
others' video and still allows screen sharing. A forwarder limits incoming layers; a mesh
call currently receives all layers the others send. Auto never turns your camera off.
After leaving Audio only, use Start video if you want the camera back on.

**Hide my address** in Connection uses relay-only links when you join. It does not hide a
host's address, the relay still sees yours, and a short invite fetch also reveals your
address to the server serving it. It needs a usable relay. See
[what other parties can see](TRANSPARENCY.md#2-what-other-parties-can-see).

## If a call does not connect

| Symptom | What to check |
|---|---|
| A used invite is refused | Ask the host for a fresh link; a previous knock may have consumed it |
| An invite fails to parse | Update both devices; older versions cannot read short links |
| Guests cannot reach a Windows host | Use Allow guests on the hosting copy; a rule for another executable path does not cover it |
| A phone hotspot or shared provider address cannot host | Configure a publicly reachable node whose owner has allowed your device key |
| You reach the host but guests cannot reach each other | The host-run rendezvous needs its own reachable UDP port too, or use a public node |
| An office network blocks all UDP | Veil has no TCP/TLS fallback yet; it needs a network that carries UDP |
| Video briefly stops after a connection drop | Veil retries missing media links with backoff. Check the link status and log; long outages may still need a fresh invite |

On Windows, logs are in `%LOCALAPPDATA%\Veil\logs\veil.log`. Include the versions, operating
systems and relevant log lines when reporting a bug, and remove invite links or addresses
you do not want to share. Report security problems through the repository's Security tab,
as described in [SECURITY.md](../SECURITY.md).
