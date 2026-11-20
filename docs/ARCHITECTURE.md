# Veil architecture

Veil is a peer-to-peer video calling app written in Rust. Its desktop app, call stack and
self-hosted node are implemented on Windows, Linux and macOS. It targets 4K at 60 fps;
the current app sends camera video up to 1080p and screen shares up to 1920 pixels on their
longest side. H.264 and Opus are the current codecs.

This document includes the future design. **Current implementation** and **Planned** labels
below distinguish it from what can be used today. Test coverage and security limits are in
[TRANSPARENCY.md](TRANSPARENCY.md); [HANDOFF.md](HANDOFF.md) records development history.

Motto: **modular, agnostic, shared.**

## 1. Principles

- **Modular.** Every piece of infrastructure is a *role* behind one interface: discovery, relay,
  forwarding, identity, policy and recording. Implemented roles can be extended with adapters;
  enterprise identity, signed policy and recording are still planned.
- **Agnostic.** Any machine can fill any infrastructure role. A participant's laptop, a box you
  run at home, or an enterprise Kubernetes cluster all run the same `veil-node` code and speak
  the same protocol. Where servers live is a choice, not a dependency.
- **Shared.** One open protocol. An enterprise adds capacity and policy. It does not fork the app.
- **Infrastructure is never trusted with content.** Only participants hold media keys. Relays and
  forwarders carry ciphertext. Server choice affects capacity, availability and who sees
  connection metadata; those servers are not given the call's content keys.

## 2. Roles

| Role | Job | What it can see | Adapters |
|---|---|---|---|
| Participant | Captures, encodes, decodes, holds keys | Plaintext media | Desktop app |
| Rendezvous | Helps peers find each other's addresses | IP addresses, room id hash, timing | Invite link only, LAN mDNS, self-hosted rendezvous, enterprise directory |
| Relay | Carries packets when direct connection fails | Ciphertext, IP addresses | Any peer, `veil-node`, enterprise relay |
| Forwarder | Receives each stream once, sends each receiver only the layers it needs | Ciphertext, layer headers (size, keyframe flag) | Peer, `veil-node`, enterprise cluster |
| Identity | Binds a person to a device key | Public keys only | Local key with safety numbers, enterprise OIDC or SAML attestation |
| Policy | Rules for a room | Configuration | Local defaults, signed enterprise policy |
| Recorder | Compliance recording (opt-in) | Plaintext, because it is a member | Enterprise only, always visible to everyone |

**Current implementation:** desktop participants, peer-hosted or self-hosted rendezvous,
relay and one forwarder per room, local device keys and safety codes, and local policy.
LAN mDNS, enterprise directories, identity attestations, signed organisation policy,
recording and forwarder clusters/cascades are **planned**.

## 3. Bandwidth

The topology library can choose layouts from supplied capacities. The desktop currently
supplies assumed capacities: mesh through four people, one configured or host-run forwarder
above that (`veil-app/src/engine/plan.rs`). It does not measure capacity to choose its layout.

### Current implementation

| Choice | Camera sent | Sound sent | Video received |
|---|---|---|---|
| Auto | 640-wide thumbnail at half capture rate, plus the camera size up to 1920 wide | Opus, constant bitrate, 32 kbit/s before overhead | Layers requested for tile size through a forwarder; every sent layer in mesh |
| Data saver | One 320-wide layer at half capture rate, about 150 kbit/s | 16 kbit/s before overhead | Smallest requested layer through a forwarder; every sent layer in mesh |
| Audio only | No camera video | 16 kbit/s before overhead | Others' video still arrives; smallest requested layer through a forwarder |

Auto uses Data saver after three bad seconds within five. It returns after 15 good seconds,
with longer holds after repeated failures. It has two steps and never chooses Audio only.
This uses measured loss/delay/rate changes, without a capacity probe before stepping up
(`engine/auto_step.rs`). A shared screen is separate from the camera and remains available
in Audio only.

### 3.1 Target bitrate ladder (planned beyond today's desktop limits)

| Layer | Resolution | Bitrate (AV1 or HEVC) |
|---|---|---|
| Uhd | 2160p60 | about 20 Mbps |
| Hd | 1080p30 | about 3 Mbps |
| Thumb | 360p15 | about 0.5 Mbps |
| Audio | Opus | about 48 kbps with overhead |

### 3.2 Mesh versus forwarded

**Mesh.** Every participant sends to every other participant. No extra hop, lowest latency.
The cost grows with each person: in a 4-person call the speaker uploads 3 x 20 = 60 Mbps for 4K.

**Forwarded.** Each participant uploads *once* to a forwarder. The forwarder sends each receiver
only what it subscribed to. Upload no longer depends on call size.

Hypothetical target example: 16 people, one speaker in 4K, everyone else as thumbnails.
The current app neither sends this 4K layer nor uses cascaded forwarders.

| | Mesh | One forwarder | Four cascaded forwarders |
|---|---|---|---|
| Speaker upload | 300 Mbps (fails) | 20 Mbps | 20 Mbps |
| Others upload | 7.5 Mbps | 0.55 Mbps | 0.55 Mbps |
| Each receiver download | 27.7 Mbps | 27.7 Mbps | 27.7 Mbps |
| Busiest forwarder upload | n/a | about 425 Mbps | about 110 to 175 Mbps |

The forwarder does the heavy lifting. That is why where the forwarder runs is the important
choice, and why the design lets anyone supply one.

### 3.3 Bandwidth design and implementation status

1. **Simulcast (built).** H.264 uses separate encoders per resolution rung and temporal layers.
   A forwarder drops layers using the authenticated, readable marker. AV1/HEVC are planned.
2. **Subscribe to what you render.** Each receiver asks for the layer that matches its tile
   size. In a 16-person grid the tiles are small, so most streams are thumbnails. Only the active
   speaker or a pinned person asks for the larger current rung. The app has two size classes;
   mesh subscriptions and a true no-video request are still missing.
3. **Layer pausing (partial).** The media scheduler can pause unwanted layers. Aggregating
   receivers' subscriptions into sender feedback is planned; currently the forwarder passes
   every spatial layer up to the requested ceiling, including redundant simulcast layers.
4. **Pick the forwarder by capacity (library only).** The planner uses supplied capacities.
   Measurement and selection of peer forwarders in the desktop are planned.
5. **Cascading (planned transport).** When one forwarder is not enough, receivers are split across several
   forwarders. Forwarders exchange each stream once. One 425 Mbps job becomes four jobs of
   about 110 to 175 Mbps. The same mechanism places one forwarder per office or region.
6. **Audio stays cheap (built).** Constant-bitrate Opus, in-band forward error correction and a
   jitter buffer keep sound independent of the video ladder. Discontinuous transmission is
   not the current privacy-preserving constant-rate choice.
7. **Content-aware screen encoding (planned).** The current fixed-rate screen path repeats a
   still picture. Its limits are in section 3.5.
8. **Congestion control (partial).** QUIC uses quinn's loss-based default. Auto reacts to bad
   links; delay-based transport control and measured recovery probes remain planned. Lost
   video recovers through keyframes; audio uses FEC and concealment.
9. **Locality (built addresses, planned discovery).** Direct LAN addresses work today. mDNS
   discovery is planned. The node you configure determines where fallback traffic goes.
10. **Graceful degradation (partial).** The planner can degrade a supplied layout. The desktop
    currently uses the two-step Auto policy above, rather than the full 4K-to-audio design.

### 3.4 Peer-hosted versus bring-your-own compute

The forwarder role is the same code wherever it runs:

| Where it runs | Good for | Limit |
|---|---|---|
| A participant | Friends, small teams, no servers at all | Their upload, their machine stays on |
| A self-hosted `veil-node` | A home server or a small business | One box's bandwidth |
| An enterprise cluster | Large organisations, many rooms at once | Whatever they provision |

The planner's policy can prefer different node kinds. The desktop currently uses the
configured forwarder or starts one on the host for larger calls. Enterprise policy delivery,
clusters and federation are planned. No default public node address is shipped in the app.

### 3.5 Screen sharing today

A shared screen is a second video stream from the same person, beside their camera. Item 7 in
section 3.3 is the goal. What runs now is simpler:

- The sender has a second encoder with one layer. The picture is at most 1,920 pixels on its
  longest side and uses at most 4 Mbit/s.
- Up to 30 pictures a second go out. When the screen is still, the last picture is sent again,
  so those watching know that the share goes on.
- A flag inside the sealed video header marks the stream as a screen. Receivers decode camera
  and screen apart. A forwarder sees one more stream and cannot tell that it is a screen.
- Each screen picture carries a signature from the sender's device key, inside the sealed
  frame. A picture that the named member's device did not sign is dropped.
- A receiver treats a share that has been silent for 3 seconds as ended.
- Data saver and Audio only cap a share at 1280 pixels on its longest side and 600 kbit/s.
- Picture numbers continue when an encoder reopens, so selecting a new area or changing
  Data use does not freeze the receiver's decode gate.

Windows captures through Windows Graphics Capture. X11 and macOS take repeated screenshots
through `xcap`. On KDE Plasma's Wayland session KWin lists windows, takes the thumbnail and
preview screenshots and streams the share (`veil-media/src/screen/kde.rs`), so Veil's own
picker works there. Other Wayland sessions use the desktop's screen cast portal: the desktop's
own chooser picks the source, and its PipeWire stream feeds both the area preview and the share
(`veil-media/src/screen/wayland.rs`). ADR 0005 has the reasons. `xcap` is not used there, because it would write each
picture to a file.

## 4. Enterprise integration (planned)

The role interfaces and `veil-node` binary exist. The enterprise behaviours below describe
the intended extension, not deployable features. There is no published container image in
this repository's release workflow.

- **One deployable.** `veil-node` is one binary; a container image is planned. Configuration turns
  roles on or off: relay, forwarder, rendezvous. It runs on Kubernetes, virtual machines, bare
  metal or on premises.
- **Signed policy.** The organisation signs a policy document with its organisation key. It sets
  allowed forwarders and relays, recording rules, retention, minimum client version, and whether
  identities must be attested by the company identity provider. Clients refuse rooms whose
  policy signature does not verify.
- **Identity.** A person signs in with OIDC or SAML. The identity provider issues a short-lived
  attestation that signs the device's public key. Keys are still generated on the device. The
  identity provider never holds them. Removing a person creates a new key epoch that locks them
  out.
- **Observability.** Nodes export metrics through OpenTelemetry: bitrates, loss, round-trip time,
  load. No content, no keys.
- **Compliance recording.** A recorder joins as a group member. Everyone sees it in the roster,
  with a banner. There is no silent recording path, by design.
- **Federation.** Two organisations each bring their own nodes. Their forwarders cascade to each
  other. Each side's policy applies to its own users.
- **Gateways (later).** Bridges to SIP or H.323 room systems and to browsers through WebRTC. A
  gateway decrypts, so it is a participant: visible, and controlled by policy.

## 5. Security

### 5.1 Layers

1. **Transport.** QUIC with TLS 1.3 on every hop, pinned to device keys. Control contents are
   protected; IP addresses, packet sizes and timing remain visible to the network.
2. **Group keys.** MLS (RFC 9420) through `openmls`. Forward secrecy and post-compromise
   security. Every join or leave starts a new epoch. Each member's MLS signature key is its
   device key, so the roster matches the safety numbers people compare. For now the host is the
   only member allowed to commit; members refuse commits from anyone else.
3. **Media.** Per-frame authenticated encryption, in the style of SFrame (RFC 9605). Each sender
   has its own key for each epoch, derived from the MLS exporter secret. Forwarders read only
   the plaintext layer header. Every member can derive every sender's key, so frame keys
   prove a frame came from a member, not which one. Display names and chat are sealed the
   same way, with the sender's frame key, behind a fixed prefix so a forwarder cannot pass
   one off as video or sound, and each is also signed with the sender's device key over its
   frame id, so no member can send one as another; forwarders pass them on unread.
4. **Identity.** Ed25519 device keys and pair/group safety codes compared through a trusted
   external channel. Windows persists keys with DPAPI; Linux/macOS keys last one run.
   Identity-provider attestation is planned.
5. **Invites.** Signed by the host, with an expiry and a one-time join token. An invite carries
   no media key. The host still admits each joiner from a lobby. A short link holds the
   rendezvous addresses and a 128-bit secret. The rendezvous keeps the signed invite sealed
   under that secret for as long as the host's room is open, and the guest verifies it as usual
   after opening it (`veil_security::short_invite`).

### 5.2 Threat model

| Attacker controls | Can do | Cannot do |
|---|---|---|
| The network | See who talks to which node, packet sizes and timing | Read or change media |
| A relay or forwarder | Drop or delay packets, see metadata | Read media, inject or replay frames |
| A stolen invite link | Ask to join | Join without host approval, reuse the token |
| A compromised device | Read and retain plaintext while it is a member | Use its old keys to decrypt new frames after removal and rekeying |
| An identity provider (planned) | Refuse to attest someone | Read media or impersonate a key people verified |

**Known limits:** packet sizes and timing reveal activity. Padding is planned and cannot be
turned on today. Through a forwarder a malicious member can still impersonate another
member's camera/audio or disrupt media replay windows. Names, chat, hands and reactions have
device signatures; screen pictures are signed too, though media replay disruption still
applies. See [TRANSPARENCY.md](TRANSPARENCY.md#2-what-other-parties-can-see).

The host is the only MLS committer, so the call ends when the host leaves. Old media keys
have a two-second grace period on ordinary rekeys, with no grace when a member is removed.

## 6. Code layout

| Crate | Responsibility |
|---|---|
| `veil-app` | Desktop UI, saved choices, call engine, sharing controls and verified updates |
| `veil-call` | Participant sessions, lobby events, signed plans, media routes, redial and signed app messages |
| `veil-security` | Device keys, invites, pair/group safety codes, per-frame encryption and replay windows |
| `veil-group` | MLS group state, host admission and a crypto provider using permissively licensed primitives |
| `veil-net` | Pinned QUIC links, bounded control framing, video streams, audio datagrams and address families |
| `veil-rendezvous` | Address reflection, consented introductions, relay sessions, short invites and reachability checks |
| `veil-forward` | Host-signed roster tickets, encrypted forwarding, layer subscriptions and bounded queues |
| `veil-topology` | Pure layout planning from supplied nodes, policy and capacities, including future cascade layouts |
| `veil-media` | OS capture/playback, H.264 and Opus, jitter/decode scheduling, blur, filters and noise suppression |
| `veil-portmap` | Opt-in PCP, NAT-PMP and UPnP router mappings, plus PCP IPv6 pinholes |
| `veil-node` | Configured rendezvous/relay and forwarder roles, with optional host device-key allow-lists |
| `veil-probe` | Camera-free real-network calls, paths and round-trip/reply statistics |

### Runtime and connection recovery

The screens read `model.rs`. The app's engine uses a Tokio runtime for call/session work
and worker threads for camera encoding, screen encoding, video decoding and audio playback.
Local capture and effects have their own workers. Bounded queues drop stale media rather
than blocking the interface or retaining an unlimited backlog.

The host distributes a signed plan after each MLS epoch. Direct links begin at once,
hole-punch attempts after one second and relayed attempts after five, or sooner if earlier
paths fail. A relay-only joiner uses only the relay. IPv6 candidates begin first, with
IPv4 following after 250 ms. Forwarder connections currently have no relay fallback.

A missing media link is redialled on the session tick by the designated side, with waits
of 1, 2, 4, 8 and then 16 seconds after failures. A result is adopted only while its target
and ticket still match the plan and the session still wants the peer. A returned link
refreshes subscriptions, keyframes and name/hand announcements. This repairs media links;
losing the host's control link can still end membership and require a fresh invite.

### Implementation boundaries

The workspace forbids Veil-owned `unsafe` outside the media crate's documented FFI modules.
Dependency and model licences are recorded in [DEPENDENCIES.md](DEPENDENCIES.md), generated
by `python tools/licences.py`. Native code in dependencies remains part of the runtime and
attack surface; safe Rust at the app layer is not a claim that the entire binary is safe Rust.

Windows media uses Media Foundation and WASAPI. Linux uses Video4Linux2 and ALSA; macOS uses
AVFoundation and Core Audio. Linux/macOS H.264 uses OpenH264 built from source, with the
patent-cover limitation documented in [TRANSPARENCY.md](TRANSPARENCY.md). OS hardware codec
back ends there remain future work. There is no FFmpeg dependency.
