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
   the plaintext layer header.
4. **Identity.** Ed25519 device keys. Safety numbers people can compare. Optional identity
   provider attestation.
5. **Invites.** Signed by the host, with an expiry and a one-time join token. An invite carries
   no media key. The host still admits each joiner from a lobby.

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
| `veil-net` | Built | QUIC links (`quinn`) pinned to device keys with TLS 1.3 raw public keys; control stream; one stream per video frame; audio datagrams; parallel dialling of candidate addresses; `NodeAddr` (`key@ip:port`), the text form that pins a node's key in invites |
| `veil-rendezvous` | Built | Runs on any node. Reflects public addresses, introduces two members of a room for a timed simultaneous dial (hole punching), and relays. Rooms are registered by hash, never by id. A relayed link is the two members' own pinned QUIC link running inside the relay session, so the relay carries ciphertext only |
| `veil-group` | Built | MLS group state (`openmls`) and the host's lobby. The host checks each joiner's one-time invite token, waits for approval, then adds the joiner's key package, which must be signed by the device key the QUIC link is pinned to. Every join or leave is a commit and a new epoch. Each epoch's 32-byte exporter secret keys `veil-security` frames; a member's frame sender index is its MLS leaf index. Runs on Veil's own security backend (one ciphersuite: X25519, ChaCha20-Poly1305, SHA-256, Ed25519) because `openmls_rust_crypto` depends on MPL-2.0 code |
| `veil-media` | Core and Windows encoder built | Built: the bitrate ladder, the per-picture schedule (temporal pattern, keyframes on pattern boundaries, layer pausing), the encrypted video header, the receiver's decode gate that asks for keyframes, and H.264 simulcast on Windows: one Media Foundation encoder per rung (GPU first, Microsoft's software encoder as fallback), each running the rung's temporal layers, fed a box-scaled copy of the picture. Next: camera capture, decoding, VideoToolbox (macOS), VA-API (Linux), Opus audio. Native calls live only in one `ffi` module per OS |
| `veil-call` | Built | Participant sessions. Every participant starts a `CallNode`: one accept loop routes each incoming link by its first control message (`Hello` to the host session, `Media` to the media session), and a presence task on the call's rendezvous node handles introductions and relay sessions. The host runs one actor task that owns the MLS group and lobby: `Hello` raises a knock, the host decides, the key package becomes a Welcome for the joiner and a commit for everyone else. A closed link is a leave. `join` dials the invite's direct addresses, then a punched dial, then a relay session on the invite's relay node. The media session follows the `veil-topology` plan: a link to every other member (the lower device key dials, by direct, punched or relayed path) or one link to the home forwarder. A `KeyRing` keeps the previous epoch's receiver for 2 seconds after a change. A relay-only mode never reveals a participant's address to the others. Runs on participants only |
| `veil-forward` | Built | The forwarder role as a library, so a participant or a node can run it. Members join a room by its tag; each sealed video frame and audio packet goes unchanged to every other member, and each receiver subscribes to the highest spatial and temporal layer it wants per sender, read from the frame's plaintext layer marker (raises wait for a keyframe, and the forwarder asks the sender for one). A member's keyframe request goes on to the named sender only, without the asker's identity. Per-receiver queues are bounded in bytes, so a slow receiver loses frames instead of stalling the room |
| `veil-node` | Built | One binary. A TOML file turns on the rendezvous role (with or without relay) and the forwarder role, each on its own port with the node's one device key, created on first start. Prints each role as `key@address`. Holds no group or media keys |
| `veil-app` | Next | Desktop interface (`egui` and `wgpu`) |

Every dependency has its licence checked before it is added (`python tools/licences.py`, see
[TRANSPARENCY.md](TRANSPARENCY.md)). FFmpeg (LGPL) is avoided; hardware codecs are reached
through the operating system's own media APIs.
