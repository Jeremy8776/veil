# Veil architecture

Veil is a peer-to-peer video calling app written in Rust. It targets 4K at 60 fps, low latency
and end-to-end encryption, with no reliance on cloud compute.

Motto: **modular, agnostic, shared.**

## 1. Principles

- **Modular.** Every piece of infrastructure is a *role* behind one interface: discovery, relay,
  forwarding, identity, policy and recording. Each role has more than one adapter.
- **Agnostic.** Any machine can fill any infrastructure role. A participant's laptop, a box you
  run at home, or an enterprise Kubernetes cluster all run the same `veil-node` code and speak
  the same protocol. Where servers live is a choice, not a dependency.
- **Shared.** One open protocol. An enterprise adds capacity and policy. It does not fork the app.
- **Infrastructure is never trusted with content.** Only participants hold media keys. Relays and
  forwarders carry ciphertext. So choosing your own servers is a capacity and policy decision. It
  never weakens security.

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

## 3. Bandwidth

Bandwidth is arithmetic, not code. A call works when every link carries no more than its
measured capacity. The planner (`veil-topology`) does this sum for every call and picks the
cheapest layout that fits.

### 3.1 Bitrate ladder

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

Worked example: 16 people, one speaker in 4K, everyone else as thumbnails.

| | Mesh | One forwarder | Four cascaded forwarders |
|---|---|---|---|
| Speaker upload | 300 Mbps (fails) | 20 Mbps | 20 Mbps |
| Others upload | 7.5 Mbps | 0.55 Mbps | 0.55 Mbps |
| Each receiver download | 27.7 Mbps | 27.7 Mbps | 27.7 Mbps |
| Busiest forwarder upload | n/a | about 425 Mbps | about 110 to 175 Mbps |

The forwarder does the heavy lifting. That is why where the forwarder runs is the important
choice, and why the design lets anyone supply one.

### 3.3 Techniques that cut bandwidth

1. **Layered encoding.** Each sender encodes once with AV1 SVC or simulcast. One stream carries
   Thumb, Hd and Uhd layers. A forwarder drops layers without decrypting anything.
2. **Subscribe to what you render.** Each receiver asks for the layer that matches its tile
   size. In a 16-person grid the tiles are small, so most streams are thumbnails. Only the active
   speaker, a pinned person or a screen share gets 4K.
3. **Layer pausing.** The forwarder tells each sender which layers nobody wants. The sender stops
   encoding them. This saves upload *and* encoder load. A silent person in a big call uploads about
   0.5 Mbps.
4. **Pick the forwarder by capacity.** The planner measures each candidate's spare upload and
   chooses the best one allowed by policy. A participant on 1 Gbps fibre can host a 16-person call.
5. **Cascading.** When one forwarder is not enough, receivers are split across several
   forwarders. Forwarders exchange each stream once. One 425 Mbps job becomes four jobs of
   about 110 to 175 Mbps. The same mechanism places one forwarder per office or region.
6. **Audio stays cheap.** Opus with discontinuous transmission. Audio always reaches everyone,
   even when video has to degrade.
7. **Content-aware encoding.** Screen share uses full resolution and drops frame rate when the
   screen is still. A camera rarely needs 4K60, so 4K60 is reserved for where it shows.
8. **Per-link congestion control.** Delay-based bandwidth estimation on every hop. When a link
   shrinks, the receiver drops a layer. The call does not drop. Audio uses forward error
   correction; video uses retransmission requests and keyframe requests.
9. **Locality.** Two people in the same office connect over the LAN. An on-premises
   `veil-node` keeps office traffic inside the building.
10. **Graceful degradation.** When nothing fits, the planner lowers the highest layer in use
    (Uhd to Hd, then Hd to Thumb) and reports that the call is degraded. It never silently fails.

### 3.4 Peer-hosted versus bring-your-own compute

The forwarder role is the same code wherever it runs:

| Where it runs | Good for | Limit |
|---|---|---|
| A participant | Friends, small teams, no servers at all | Their upload, their machine stays on |
| A self-hosted `veil-node` | A home server or a small business | One box's bandwidth |
| An enterprise cluster | Large organisations, many rooms at once | Whatever they provision |

Policy chooses the order of preference. The default is: enterprise node, then self-hosted node,
then a participant. An organisation can forbid participant forwarding entirely ("our servers
only"), or forbid servers entirely ("peers only").

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

Windows captures through Windows Graphics Capture. X11 and macOS take repeated screenshots
through `xcap`. Linux Wayland sessions use the desktop's screen cast portal: the desktop's own
chooser picks the source, and its PipeWire stream feeds both the area preview and the share
(`veil-media/src/screen/wayland.rs`). `xcap` is not used there, because it would write each
picture to a file.

## 4. Enterprise integration

- **One deployable.** `veil-node` is one binary and one container image. Configuration turns
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

1. **Transport.** QUIC with TLS 1.3 on every hop. Node keys are pinned. This protects metadata
   from the network.
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
4. **Identity.** Ed25519 device keys. Safety numbers people can compare. Optional identity
   provider attestation.
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
| A compromised device | Read calls while it is a member | Read calls after removal (new epoch) |
| An identity provider | Refuse to attest someone | Read media or impersonate a key people verified |

**Known leak:** packet sizes show when someone is speaking or moving. High-security rooms can
turn on padding, at a bandwidth cost.

## 6. Code layout

| Crate | Status | Job |
|---|---|---|
| `veil-security` | Built | Device identity, safety numbers, signed invites, per-frame encryption |
| `veil-topology` | Built | Planner: mesh or forwarded, forwarder choice, cascades, degradation |
| `veil-net` | Built | QUIC links (`quinn`) pinned to device keys with TLS 1.3 raw public keys; control stream; one stream per video frame; audio datagrams; dialling of candidate addresses, IPv6 first and IPv4 250 ms later; one dual-stack socket for both families; `NodeAddr` (`key@ip:port`), the text form that pins a node's key in invites; `NodeName` (`key@host:port`), a node named by DNS in settings, resolved when a call starts |
| `veil-rendezvous` | Built | Runs on any node. Reflects public addresses, introduces two members of a room for a timed simultaneous dial (hole punching), and relays. Rooms are registered by hash, never by id, and named by the host's key: only the host opens a room, and only keys on its allow list (members and lobby joiners) or holding a lobby pass from an open invite may register, so people it declined or removed are dropped. Relayed bytes are capped per session and in total. A relayed link is the two members' own pinned QUIC link running inside the relay session, so the relay carries ciphertext only |
| `veil-group` | Built | MLS group state (`openmls`) and the host's lobby. The host checks each joiner's one-time invite token, waits for approval, then adds the joiner's key package, which must be signed by the device key the QUIC link is pinned to. Every join or leave is a commit and a new epoch. Each epoch's 32-byte exporter secret keys `veil-security` frames; a member's frame sender index is its MLS leaf index. Runs on Veil's own security backend (one ciphersuite: X25519, ChaCha20-Poly1305, SHA-256, Ed25519) because `openmls_rust_crypto` depends on MPL-2.0 code |
| `veil-media` | Core, Windows, Linux and macOS video and audio, Opus, jitter buffer built | Built: the bitrate ladder, the per-picture schedule (temporal pattern, keyframes on pattern boundaries, layer pausing), the encrypted video header, the receiver's decode gate that asks for keyframes, and H.264 simulcast on Windows: one Media Foundation encoder per rung (GPU first, Microsoft's software encoder as fallback), each running the rung's temporal layers, fed a box-scaled copy of the picture. Also built: Filters (warm, cool, black and white, bright) and Background blur applied to the camera picture in NV12 before the self-view and the encoder (blur: Veil's own safe-Rust run of the MediaPipe selfie segmenter on a worker thread, embedded weights; see ADR 0001), Windows camera capture to NV12, the Windows H.264 decoder (Microsoft's, on the CPU, low latency) and Opus audio, the audio jitter buffer, and Windows microphone and speakers (WASAPI). On Linux and macOS: H.264 with OpenH264 built from source (same simulcast and temporal layers; see TRANSPARENCY.md about patents), microphone and speakers through `cpal` (ALSA, Core Audio), and the camera through Video4Linux2 (`v4l`, NV12, YUYV or MJPEG) or AVFoundation. `platform.rs` names each OS's back end once for the app. Later: VideoToolbox and VA-API hardware encoding. Native calls live only in the `ffi` modules |
| `veil-call` | Built | Participant sessions. Every participant starts a `CallNode`: one accept loop routes each incoming link by its first control message (`Hello` to the host session, `Media` to the media session), and a presence task on the call's rendezvous node handles introductions and relay sessions. The host runs one actor task that owns the MLS group and lobby: `Hello` raises a knock, the host decides, the key package becomes a Welcome for the joiner and a commit for everyone else. A closed link is a leave. `join` dials the invite's direct addresses, adds a punched dial after 1 second and a relay session on the invite's relay node after 5 seconds, sooner if earlier paths fail, and uses the first link up (`dial.rs`). The media session follows the `veil-topology` plan: a link to every other member (the lower device key dials, by direct, punched or relayed path) or one link to the home forwarder. A `KeyRing` keeps the previous epoch's receiver for 2 seconds after a change. A relay-only mode never reveals a participant's address to the others. Members send their display names and chat as sealed app messages (`app_msg.rs`: checked lengths and characters, at most 16 KiB sealed) on each media link's control stream, or through the forwarder. Runs on participants only |
| `veil-forward` | Built | The forwarder role as a library, so a participant or a node can run it. Members join a room by its tag; each sealed video frame and audio packet goes unchanged to every other member, and each receiver subscribes to the highest spatial and temporal layer it wants per sender, read from the frame's plaintext layer marker (raises wait for a keyframe, and the forwarder asks the sender for one). A member's keyframe request goes on to the named sender only, without the asker's identity. A sealed app message (names, chat) goes unchanged to every other member. Per-receiver queues are bounded in bytes, so a slow receiver loses frames instead of stalling the room. A forwarder can be limited to one host's tickets |
| `veil-node` | Built | One binary. A TOML file turns on the rendezvous role (with or without relay) and the forwarder role, each on its own port with the node's one device key, created on first start. Prints each role as `key@address`. Holds no group or media keys |
| `veil-portmap` | Built | Opens UDP ports on the home router for a hosted call (the call's port and, without a configured node, the host's own rendezvous port), only when the person allowed it: PCP, then NAT-PMP, then UPnP IGD. Short leases renewed during the call and removed at its end. Talks only to the default gateway (or, for UPnP, the device that answered from it, at the address it answered from). A router whose external address is not public is reported as a second NAT (carrier-grade NAT), since its mapping opens nothing. For IPv6, which has no NAT, a PCP pinhole lets the call's port through the router's firewall to this machine's public IPv6 address |
| `veil-app` | Built, Windows; Linux and macOS builds untested on real devices | Desktop interface on `eframe` with `wgpu`, wired to real calls. Home (name, new call, join by invite link, a labelled demo call), pre-join (live camera preview, microphone meter, device pickers), and the call: gallery or speaker view, a control bar, and side panels for people with the host's lobby (admit, decline, remove), chat (sealed and sent to everyone in the call), and safety numbers with each link's path. The screens read only `model.rs`. `engine/` runs the call off the interface thread: a Tokio runtime for `veil-call` (host: group, lobby, a fresh one-time invite after each knock, a mesh plan published after every epoch; guest: join, lobby wait, keys and routes), and three threads for media. Camera pictures go through a `Schedule` and the Media Foundation simulcast encoder (a 640-wide thumbnail at half rate plus the camera's size up to 1080p), get a `VideoHeader`, are sealed with `encrypt_layer` and sent; received frames are opened through a `KeyRing`, pass a per-sender `Gate` and are decoded (one spatial layer per sender, the largest arriving). Microphone frames go to Opus with an `AudioHeader` as sealed datagrams; received packets go through a `JitterBuffer` and `OpusDecoder` per sender, are mixed and played with WASAPI. Keyframe requests go both ways, and so do display names (each member's own, sent after every epoch change) and chat. Without a configured `veil-node` the host runs its own rendezvous, relay and forwarder for the call, serving that call only, which guests must reach, so on Windows the host's pre-join screen checks the firewall and offers "Allow guests" (one inbound UDP rule for `veil-app.exe`, confirmed in Windows' administrator prompt) |

Every dependency has its licence checked before it is added (`python tools/licences.py`, see
[TRANSPARENCY.md](TRANSPARENCY.md)). FFmpeg (LGPL) is avoided; hardware codecs are reached
through the operating system's own media APIs.
