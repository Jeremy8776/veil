# Veil roadmap

Updated 23 November 2026. Proposed milestones for maintainer review; versions are planning
targets, not release dates or changes to the current package version.

**Modular. Agnostic. Shared.** Veil is a video and audio calling app and a reusable call
engine/provider that other applications can embed. Its standalone app is a first-party client
of the same calling capabilities offered to integrators.

Veil's goal is familiar, dependable private communication where people own their identities,
contacts and conversation data, calls are peer to peer by default, and a surviving local
network remains useful when the internet fails. People may add their own computers or chosen
nodes for reach and capacity. Those helpers must be replaceable and receive no conversation keys.

The product should feel as approachable as a conventional calling app without requiring a
company account, company contact directory or company-operated call infrastructure. Easier
connection must preserve strict admission, verification and revocation. Android delivery is
part of reaching people; it is not the whole mission.

## Mission and session decisions

Our 20 November networking session established two requirements in Jeremy's own words:

> "Its supposed to be local, work when internet fails, peer to peer the whole shebang"

> "we must not lose sight over the mission and we must not compromise on security either"

The same session rejected building a company that stores everyone's information on a VPS.
The architecture already records the wider direction: modular roles, infrastructure of the
user's choosing, and one shared protocol from personal calls to organisation-provided capacity.
This roadmap carries those decisions forward. New release allocations and numerical test
targets below are proposals; they are not additional decisions attributed to Jeremy.

On 23 November, Jeremy clarified that other applications should be able to use Veil as their
call provider, naming Mailcraft, Discord and Slack as examples. Embeddability is therefore a
core product requirement. Compatibility with any particular third-party extension API is a
separate adapter feasibility question, not something the existence of a Rust library proves.

| Decision | Product consequence | What must not happen |
|---|---|---|
| Local operation is a primary mode | Starting and joining on a reachable LAN must work with WAN and DNS unavailable | A saved internet hostname prevents a local call from starting |
| Devices own identity and relationships | Keys and verified contacts stay locally controlled; export/recovery needs its own design | A central account or contact upload becomes necessary to call |
| Helpers are optional and replaceable | A home PC, chosen community node or organisation node can supply a role | Veil's own infrastructure becomes a hidden dependency |
| Connectivity and trust are separate | Discovery and route recovery still enforce identity, admission and current membership | Finding a device or resuming a socket silently grants access |
| Security bounds convenience | Limits, consent and understandable failures are part of every connection feature | A fallback disables pinning, encryption, privacy choices or removal |
| One protocol serves every deployment | Add roles/adapters and capacity without splitting the personal and enterprise products | An enterprise fork quietly changes the trust model |
| Other apps can use Veil as their call provider | Offer a documented engine/SDK and provider contract; the standalone app exercises the same path | Every integration copies the app's private engine or creates a new call protocol |

### The everyday experience we are building toward

Alex starts a call on a laptop. Sam joins from a phone over 5G using an invitation and waits
for admission. If they can connect directly they do; if they need a helper, it is one they
chose. Sam sees a comprehensible connection state without having to understand QUIC or NAT.

Later, both use a router with its WAN cable disconnected. Local only, a full invitation and
opt-in nearby discovery let them call without DNS, an account or an external service. If a
network changes, Veil attempts bounded recovery using the same authorised identity. Removal
still takes effect immediately. A working path and a valid member are separate requirements.

For a larger call, a willing participant or a chosen node can forward sealed media. Capacity
and reachability determine whether that helps. Moving to a forwarder must not misrepresent
who can still hear the call or allow a member to impersonate another person's media.

### Success means

- A new person can install, choose a name, share an invitation and reach the lobby without
  creating an account. A known contact's verification state survives restart.
- A real multi-device local call works with WAN and DNS unavailable, with no attempted
  internet traffic under the documented Local only policy.
- An internet call finds a permitted path where one exists, explains failure where none
  exists, and never relaxes trust checks to improve its success rate.
- Sound and control remain usable before video is improved. Network recovery does not
  create duplicate membership or revive removed people.
- Removing or replacing a helper does not destroy a user's identity or local relationships.
- The protection claims match the actual supported platforms and tested conditions.

## What the mission does and does not promise

| Situation | Desired supported experience | Hard boundary |
|---|---|---|
| Same router or hotspot, no WAN/DNS | Local invitations, admission, media and chat work within the reachable network | Client isolation or host firewalls can still prevent peer traffic |
| Phone on 5G joining a reachable desktop | Direct, punched or chosen-helper connection; the reported connection already succeeded | One successful case does not establish every carrier or route |
| Host and guest both behind difficult NAT | Use an independently reachable chosen helper when direct traversal fails | A helper behind the same unreachable boundary cannot make itself reachable |
| UDP blocked | Chosen TCP/TLS helper path covers invitation retrieval, discovery/control and media | TCP 443 alone does not guarantee access through an allow-list or traffic inspection |
| Slow or costly link | Stop unnecessary receive traffic, prioritise speech/control and show quality changes | A connection below the measured audio floor cannot sustain a live call |
| Internet outage across a community | Devices communicate through whatever local physical network survives | Discovery does not create radio range or a multi-hop network |
| Completely disconnected locations | Explain that no live path exists; optional future messages wait for contact | Encryption and peer-to-peer software cannot bridge absent connectivity |
| Malicious helper | Preserve device authentication and content encryption; offer another selected route | Helpers can observe metadata, delay/drop packets and record what they observe |
| Compromised participant | Contain access after removal and prevent attribution to another device | An admitted device can retain plaintext it was legitimately able to receive |

Veil is a private communication app with resilience features. It must not promise anonymity,
invisibility to the network, guaranteed censorship resistance or a new physical mesh. A shared
screen, recorder or gateway that exposes plaintext is a participant with explicit user-visible
consequences, never a hidden infrastructure role.

## Data ownership and trust boundaries

The rule is: devices own people and conversations; helpers keep only the bounded routing
state needed to make an authorised connection possible. "No central data company" does not
mean helpers see no information. Their observable metadata and retention must stay explicit.

| Data | Intended owner/location | Helper access and retention rule |
|---|---|---|
| Device private keys and verification state | OS-protected storage on the user's device | No upload to rendezvous, relay or forwarder |
| User-assigned contact names and relationships | Local storage, with explicit user-controlled export if implemented | No public directory, address book upload or social graph service |
| Call content and MLS state | Authorised participants; current session state is in memory | No helper receives content keys; persisted MLS recovery is a separate design |
| Conversation history, if enabled later | Encrypted local storage with deletion/backup controls | No automatic server archive |
| Full invitation | Shared intentionally by the host; signed, expiring and single use | A stolen invite can request entry, not bypass admission |
| Sealed short invitation | Temporary bounded storage on a chosen rendezvous | Helper retains ciphertext and routing metadata; full invites remain the offline option |
| Discovery beacon | Opt-in local broadcast using temporary service identity | No names, permanent identity, roster or invitation secret in public beacons |
| Node identity and configured host grants | Operator-controlled key/config files, including the persistent `hosts` allow-list | Keep the node's own private key local; explicit grant/revocation and retention policy |
| Helper registrations and routing state | Bounded helper memory for an active registration | Expire stale state; recheck configured grants; disclose identifiers/IP addresses the role must observe |
| Media in transit | Sealed participant frames in bounded transient buffers | No disk recording or content analysis as part of a relay/forwarder role |
| Diagnostics | Local redacted logs, exported explicitly by the user | No silent telemetry or automatic log upload; no keys or reusable invite secrets |

A malicious helper can keep a record despite a RAM-only implementation. Trust minimisation
reduces what it can learn; it cannot make its network observations disappear. Padding and
temporary connection identifiers require separate designs and measured overhead.

## System roles and extension boundaries

| Role | Current foundation | Work needed to serve the mission |
|---|---|---|
| Participant | Shared Rust engine; desktop and local Android integration | Native identity persistence, mobile lifecycle, clear trust/recovery states |
| Discovery and invitations | Full and short signed invites; opt-in IPv4 nearby discovery | Offline hardware evidence, camera QR scanning, bounded invitation handling and alternative nearby transports |
| Rendezvous | Introductions, chosen-host policy, registration recovery | Independent helpers, expiry/failover semantics, TCP-accessible joining |
| Relay | Opaque packet carrying over QUIC/UDP | Restricted-network transport, fairness under carrier NAT and observable capacity limits |
| Forwarder | One per room, sealed media, layer subscriptions, roster tickets | Sender attribution, receive-side demand, mixed reachability, measured capacity and failure recovery |
| Identity and policy | Device keys, safety codes, host admission and node host allow-lists | Cross-platform stable storage and local contacts; optional organisation attestations later |
| Distribution | Desktop packages and signed release manifests; local debug APKs | Reproducible Android CI, upgrade identity tests, OS signing and offline-verifiable installation guidance |

Adapters must be evaluated behind these roles rather than making a new transport the product's
identity. The earlier session raised an Iroh experiment; it remains a comparison/design task,
not an adoption decision. Any candidate must demonstrate no mandatory public discovery/default
relay, preserved pinned identities, Local only enforcement, bounded behaviour and acceptable
mobile/recovery performance before migration is considered.

## Product journeys and failure states

| Journey | Required state sequence | Security or usability rule |
|---|---|---|
| Start/join a call | Setup → pre-join → connecting → lobby → admitted/live | Names and discoverability do not prove identity; capture and admission states remain visible |
| Verify a person | Unverified → compared through a trusted channel → verified | A key change requires a clear warning; an identical display name never restores trust |
| Nearby join | Explicit scan/advertise → code/full invite → signed validation → lobby | Discovery grants no membership; manual full-invite joining survives blocked multicast |
| Interrupted call | Live → reconnecting → recovered, or ended with rejoin guidance | Recovery proves current membership and the same identity; it is not fresh automatic admission |
| Removed member | Live/reconnecting → removed → access revoked | An old address, ticket, invitation or recovery token never restores membership |
| Route change | Current permitted route → replacement validated → active route updated | Honour Local only and Hide my address throughout; do not silently expose a real address |
| Poor network | Video quality reduced → explicit receive limits/audio option → measured recovery | Prioritise audio/control, explain the change and respect user consent before disabling video automatically |
| Node unavailable | Detect → try another configured eligible helper → recover or explain | Another IP for the same key is not proof of an independent helper |
| Host leaves | Everyone is told the current call ended | Until handover is implemented, do not imply a group survives coordinator loss |

Never label a call simply "connected" when a participant has no usable media path. Distinguish
transport reachability, lobby/admission, cryptographic membership and media delivery in the
model, with concise product language rather than implementation jargon on the main screens.

## Embeddable call platform

### What Modular Agnostic Shared means for integrations

**Modular:** identity/storage, transport, discovery, session coordination, media, forwarding,
policy and UI have explicit interfaces. An application can supply its own UI and supported
device/storage adapters without reimplementing admission, encryption or membership.

**Agnostic:** the engine is not coupled to Veil's window, one UI toolkit, one server operator
or one host application's account model. Native Rust is the first integration surface;
cross-language hosts need a deliberate binding or local process bridge. Browser support is
a separate runtime/media design, not an automatic consequence of exporting an SDK.

**Shared:** standalone Veil, Mailcraft and other adapters use the same supported core and
interoperable protocol. They can share a call without sharing account databases. Shared
code does not mean automatically shared private keys, contacts, microphone access or history.

### Proposed layers

| Layer | Owns | Must not require |
|---|---|---|
| Security and protocol core | Device authentication, invitations, MLS membership, revocation, frame protection and replay checks | A GUI, central login or host-managed cryptographic state |
| Call engine and SDK | Session lifecycle, route policy, media orchestration, bounded commands/events, resource teardown | `VeilApp`, egui screens, global application settings or an updater |
| Platform adapters | Permission-aware camera/audio, secure storage and lifecycle integration | A separate wire protocol for each platform |
| Provider adapter | Maps the host's start/join/mute/admit/end actions and events into the SDK | Access to another app's keys or exemption from admission rules |
| Optional call UI | Video surfaces, controls, roster, verification and connection states | Replacement of the host's whole interface |
| Host application | Its user experience, authorised user actions, account mapping and data policy | A Veil company account or automatic upload of its contacts |

Current `veil-call` already exposes reusable session components. However, the higher-level
engine is private inside `veil-app` and coordinates app models/media; the Android library
entry points launch the application UI. This is a foundation for an SDK, not a documented
drop-in provider yet. Workspace packages are currently marked `publish = false`.

### Proposed provider contract

These are responsibilities to design, not names of existing public SDK methods.

| Host request | Engine obligation | Observable result |
|---|---|---|
| Create a call or consume an invitation | Validate configuration and policy; create isolated session state | Connecting/lobby/live states, actionable error, opaque session handle |
| Admit, decline or remove a person | Check coordinator authority and current membership | Roster/epoch update; removed devices remain revoked |
| Enable/disable microphone, camera or share | Enforce OS permissions and explicit capture intent | Actual device/capture state and failure reason, not merely a toggled UI |
| Select devices or a receive/data policy | Apply bounded media changes without corrupting the session | Routing/quality state, device error or confirmation |
| Set Local only or relay-only privacy | Enforce the policy on all discovery/control/media paths | Permitted route information and explicit incompatibility errors |
| Verify a device | Bind verification to its actual key through a trusted comparison flow | Verification state/key-change events scoped to that identity |
| Subscribe to state and render media | Provide ordered/versioned events and bounded queues/surfaces | Roster, lobby, media, recovery, route and terminal events |
| Cancel, leave, unload plugin or close host | Stop capture/tasks, release resources and discard session secrets | A bounded terminal transition; no orphan microphone or background service |

Commands need cancellation, idempotency rules and typed failures. Events need ordering,
backpressure, resynchronisation and thread-affinity rules. The SDK must own frame counters
and epoch transitions; adapters must not reuse session secrets or manufacture sequence state.
Concurrent sessions need explicit device arbitration and separate identity/storage namespaces.

### Integration modes and host examples

| Mode | Use | Dependency and boundary |
|---|---|---|
| Native Rust SDK | A host embeds the engine and supplies its own controls | Host runs inside the trusted endpoint process and can access plaintext/memory |
| Local provider process | Another language talks to an isolated Veil engine using authenticated local IPC | Authorise the caller, scope session handles, bound messages and clean up on host/provider crash |
| Reusable UI surface | Host uses supported call controls/video surfaces or opens the Veil call window | Preserve visible capture, verification, membership and privacy indicators |
| External app adapter | Mailcraft or a platform-supported plugin maps its UX to the provider contract | Verify that the host permits the required call actions, media surfaces and distribution |
| Protocol gateway | Interoperate with another call system when protocol bridging is necessary | A decrypting bridge is a visible participant; preserve accurate end-to-end encryption claims |

**Mailcraft reference journey:** click Call inside the host, select/share a Veil invitation,
reach the lobby, admit, talk, change devices and end the session. Join the same call from the
standalone Veil app. Veil does not need the host's mailbox, address book or message history.
The reference host is a proposed validation target, not an assertion that a Mailcraft
integration is already authorised or implemented.

**Discord/Slack-style integrations:** evaluate three distinct outcomes: launch a Veil call
from a link, embed Veil's own call experience, or bridge into a platform's native calls.
They have different APIs and trust implications. Do not promise native call-provider
replacement before checking the current platform's documented extension points and permissions.
An invite-launch integration is useful but does not alone satisfy the embedded-provider goal.

### Embedding security and compatibility

An embedding app with media or process access is a participant endpoint, not a blind relay.
It may observe or record plaintext; encryption cannot hide a user's call from software that
captures or renders it. The SDK must document this trust boundary and avoid exposing secret
keys when an opaque operation suffices. IPC limits authority but does not turn an untrusted
application receiving decrypted media into a trusted one.

Default identity stores should be scoped per host app. Reusing a person's Veil identity across
apps requires explicit consent and a design for key custody, verification continuity and
revocation; copying key files is not an integration strategy. Host account names are display
metadata until explicitly bound to a verified device identity.

Keep separate versions for the product milestone, public Rust API, IPC/binding schema and
wire protocol. Publish a compatibility matrix and a deprecation window before calling the SDK
stable. The 1.0 gate includes a supported provider contract and a standalone/embedded interop
test. A third-party platform adapter need not be complete for every named example.

## Priority and delivery approach

The critical path is **review the integrated preview → prove independent local operation and
safe recovery → widen permitted reach → preserve identity everywhere → strengthen group
capacity and continuity**. Security review, test evidence and diagnostics accompany every step.

| Priority | Work | Reason |
|---|---|---|
| Now | AND-02, SDK-01, TRUST-02, LOCAL-01, DIAG-01 and immediate TRUST-01 design | Capture the existing work, define the provider boundary, defend expanded exposure, and prove the offline premise |
| Next | MOB-01, MOB-02, INVITE-01, DIST-01 | Turn a successful connection into a usable repeatable phone/local experience |
| Then | SDK-02, NET-01, NET-02, NET-03, DATA-01, OPS-01 | Expose the shared engine and widen permitted reach while preserving privacy choices |
| Alongside platform work | DESK-01, ID-01, TEST-01, BIND-01, EMBED-01 | Stable identities, a cross-language boundary and a reference host prove shared use |
| Before broad group claims | TRUST-01, GROUP-01, MEDIA-01 | Fix attribution and explicitly handle members a forwarder cannot serve |
| Conditional extensions | IOS-01, HOST-01, NEAR-01, MSG-01, SCALE-01, PERF-01, ORG-01 | Extend the same model after feasibility, trust and resource limits are established |

This is a dependency order, not a rule that all work must be serial. Native storage can
advance while transport designs are reviewed. No prerequisite should falsely imply that
desktop networking requires an Android package. A small design/validation change belongs in
its own reviewable increment; no milestone requires a single enormous implementation PR.

## Principles

- Keep the shared Rust call engine and protocol across desktop and mobile.
- Keep calls end-to-end encrypted and servers chosen by the user. No built-in public node,
  accounts or telemetry are required by this roadmap.
- Describe a capability by the user outcome, its dependencies and evidence of completion.
  A compiled implementation, a local hardware test and an internet call establish different things.
- Treat security fixes as immediate work across milestones. A future audit does not defer
  fixing a known weakness.
- Finish and validate a useful increment before widening its platform or performance scope.

## Current evidence

| ID | Capability or test | State and evidence | Scope still open |
|---|---|---|---|
| BASE-01 | Desktop calling and self-hosted nodes | v0.1.13 published on 22 November 2026; [release](https://github.com/Jeremy8776/veil/releases/tag/v0.1.13) | Broad real-device and network coverage |
| AND-01 | Shared Android app and native integration | Implemented locally as `0.1.13-mobile.3`; touch layouts, camera, audio, Keystore and foreground call service; [mobile notes](../mobile/README.md) | Review, integration and automated Android packaging |
| TEST-LAN-01 | Phone and desktop exchange encrypted media | Hardware test on 22 November: phone video decoded, phone Opus received, desktop synthetic video and silence sent | Subjective audio quality and sustained calls |
| TEST-5G-01 | A phone on 5G connects to a desktop host | Successful test reported by Jeremy on 23 November 2026 | Test date, builds, route, duration and detailed media checks were not recorded in the report |

TEST-5G-01 is completed evidence for that connection scenario. It does not establish whether
the route was direct, punched or relayed, nor whether changing networks during a call works.
Recording those details extends the evidence; it does not undo the successful test.

The Android work is local and uncommitted in the inspected checkout. The handoff now includes
this mobile snapshot. [Architecture](ARCHITECTURE.md) describes implementation and design;
[Transparency](TRANSPARENCY.md) defines current protection limits. This roadmap owns future
priority and completion criteria; [Handoff](HANDOFF.md) owns the current session snapshot.

## Version milestones

The patch immediately after 0.1.13 consolidates the existing preview. From 0.2 onward, each
minor milestone represents a user outcome. These are proposed allocations and may be split
or reordered when testing exposes a dependency.

| Target | User outcome | Required capabilities | Exit evidence |
|---|---|---|---|
| 0.1.14 | The preview is reproducible and the provider boundary is designed | AND-02, SDK-01 | Reviewed Android integration, clean-checkout build, green desktop CI and an agreed headless engine/provider design |
| 0.2.0 | I can make a safe everyday call, including without internet | LOCAL-01, TRUST-02, MOB-01, MOB-02, INVITE-01, DIAG-01, DIST-01, TEST-01 | WAN/DNS-off hardware call, admission/revocation checks, sustained mobile calls, usable audio and signed upgradeable preview |
| 0.3.0 | Apps share a headless engine and calls find more permitted paths | SDK-02, NET-01, NET-02, NET-03, DATA-01, OPS-01 | Non-GUI SDK sample, full joining over TCP fallback, independent-helper failure tests and measured receive/data controls |
| 0.4.0 | Other apps can provide Veil calls while identity stays locally controlled | DESK-01, ID-01, BIND-01, EMBED-01 | Stable desktop identity/settings, scoped host storage, local IPC contract and standalone/reference-host interoperability |
| 0.5.0 | Group calls preserve attribution, reachability and usable media | GROUP-01, TRUST-01, MEDIA-01 | Adversarial sender tests, measured five-person calls, visible partial failures and safe forwarding/recovery |
| 0.6.0, conditional | I can join from an iPhone | IOS-01 | Signed hardware build and call/lifecycle coverage; requires Mac, Xcode and iPhone access |
| 1.0.0 | People and integrators can rely on the supported shared platform | READY-01 | Independent security review, stable provider/API contracts, supported matrix, compatibility suite and verified distribution/update paths |

The proposed 1.0 baseline requires the capabilities allocated to 0.1.14 and 0.2 through 0.5,
plus READY-01. It does not require iOS, 4K or enterprise features. Any reduction of that baseline
must be recorded here before a release candidate. The conditional iOS milestone can proceed after mobile
lifecycle and call behaviour are understood, without waiting for every desktop/group feature.

## Capability backlog

### AND-02 Integrate the Android preview

**Outcome:** Another developer can build the preview without recreating a local session.
**Status:** Reviewed integration candidate; committed build/CI gate is being verified.
**Target:** 0.1.14. **Depends on:** AND-01.

- Review the shared library extraction, Android JNI/media boundaries, key storage, permissions
  and lifecycle; keep unrelated generated files and credentials outside the change.
- Add an Android CI build/check with pinned toolchain requirements and a downloadable preview
  artifact. Keep existing desktop CI green after adding the Android workspace member.
- Document installation, exact build identity and known limitations. Maintain the mobile
  test evidence with each preview. A fresh checkout must produce an installable ARM64 preview.

**Integration boundary:** Desktop and Android consume the existing engine without declaring
the UI launcher a public SDK. Inspect JNI reference ownership, permission prompts, Activity
teardown, capture shutdown and Keystore behaviour. Review one coherent extraction/platform
change at a time; do not publish the large local working tree as an unexplained snapshot.

The [integration review](ANDROID_INTEGRATION_REVIEW.md) records native ownership and preview
limits. Sustained background/lifecycle behaviour and stable signed upgrades remain MOB-01,
MOB-02 and DIST-01 acceptance work; AND-02 does not claim those tests from a successful build.

### MOB-01 Keep a mobile call usable through interruptions

**Outcome:** People understand what happens when their phone locks, backgrounds or changes network.
**Status:** Partly implemented; validation and recovery work remain. **Target:** 0.2.0.
**Depends on:** AND-02, TRUST-02, DIAG-01.

- Test Wi-Fi to 5G and back, Wi-Fi off/on, brief loss, app background/foreground, screen lock,
  permission denial and Activity recreation. Record which cases recover or end the call.
- Proposed recovery gate: after a network interruption of up to 10 seconds, or a Wi-Fi/5G
  change while the app/service remains alive and the host remains reachable, restore media
  within 20 seconds of usable connectivity returning, without duplicate participants or stale
  routes. This is a target to validate, not a claim about current recovery behaviour.
- Backgrounding and screen lock with permissions intact must preserve audio. Revoked
  permissions, Activity destruction and losses outside the supported recovery window must
  produce a clear ended/rejoin state when automatic recovery is unavailable.
- Keep the documented background camera policy, notification and microphone behaviour
  consistent with permissions. Leaving must stop capture and the foreground service.
- Pass three 30-minute real-media phone/desktop calls, covering LAN and 5G, plus a separate
  30-minute background-audio check. Record device, build, route, interruptions and thermal/battery
  observations. These durations are proposed acceptance targets, not results already obtained.

**Failure discipline:** A dropped transport is not a deliberate leave; removal is not a
recoverable outage. Validate roster/epoch freshness after reconnection, including a removal
that occurred while the phone was offline. Phone restart is fresh session admission, not
automatic restoration of an in-memory MLS session.

### MOB-02 Make mobile speech comfortable

**Outcome:** Both people hear clear speech using the speaker or a headset.
**Status:** Audio pipeline exists; listening and routing coverage remain. **Target:** 0.2.0.
**Depends on:** AND-02.

- Check two-way speech, double talk, speaker echo, mute, volume and default-device changes.
- Test the phone speaker, an available wired/USB headset and Bluetooth; verify unplugging or
  disconnecting does not leave the call silent or using the wrong route.
- Add or integrate echo cancellation if device processing does not provide acceptable results.
  For each tested route, record a pass/fail listening verdict from both ends: intelligible speech,
  no disruptive echo or recurring dropout, and correct routing after device changes. Fix
  blocking failures and record remaining defects alongside packet/codec checks.

### DIST-01 Distribute an upgradeable Android preview

**Outcome:** Testers can install a known build and keep their identity when updating it.
**Status:** Debug APKs exist locally. **Target:** 0.2.0. **Depends on:** AND-02.

- Establish release signing, monotonic Android `versionCode`, checksums and build provenance.
  Keep signing credentials out of source and logs.
- Test a same-package signed upgrade preserving settings, device key and verified contacts.
  Document the transition from the debug preview package; do not assume a production package
  or different signing key can upgrade it in place.
- Publish installation, permission and known-limit guidance. Store submission is a separate
  distribution decision and is not required to complete this preview milestone.

### NET-01 Connect when UDP is blocked

**Outcome:** A user with an allowed node can call from a network that blocks QUIC/UDP.
**Status:** Planned; current transport is QUIC/UDP. **Target:** 0.3.0. **Depends on:** TRUST-02.

- Design TCP/TLS relay fallback with pinned identities and end-to-end media encryption intact.
- Verify direct, punched, relay-only and UDP-blocked paths separately, including wrong server
  keys, unreachable nodes and hosts refused by a node allow-list.
- Show the selected route and a useful failure explanation without requiring packet analysis.
  Repeat successful paths across two real external networks. TEST-5G-01 covers only its
  reported scenario, not this full matrix.

**End-to-end scope:** TCP access must cover rendezvous registration/introduction, short-invite
retrieval, control and media. A media-only tunnel fails if joining still needs UDP. Define
framing, cancellation, queue/byte limits, stale video dropping and audio/control priority.
Test IPv4/IPv6 resolution and connection failures without an external DNS requirement in Local
only mode. Revalidate platform/library choices when the transport ADR is written.

**Rollout:** Initially permit an explicit restricted-network selection with clear trade-offs;
add automatic fallback only after its timeout/resource/privacy behaviour is measured. Never
claim TCP 443 is indistinguishable from permitted web traffic or guarantees an allow-list bypass.

### DATA-01 Let receivers control their data use

**Outcome:** Choosing audio only can actually stop incoming video, and small tiles avoid waste.
**Status:** Partial; current Audio only stops the local camera but still receives video.
**Target:** 0.3.0. **Depends on:** DIAG-01.

- Add an explicit no-video receive choice and mesh subscriptions; distinguish it from turning
  off the sender's camera. Define separately whether shared screens are received.
- Avoid redundant simulcast layers and propagate demand so unused layers can stop encoding.
- Measure bytes and speech continuity under sustained throttling/loss for Auto, Data saver
  and Audio only. No-video mode must stop camera payload delivery in mesh and forwarded calls;
  account separately for audio, control traffic and any explicitly requested shared screen.

**Policy:** Keep capture, send and receive controls distinct. The current Auto policy does not
disable the camera automatically. Any extension toward audio/text-only operation needs clear
consent and a visible state. Test adaptation hysteresis and measured step-up probes so a
brief improvement cannot repeatedly overload the link. Report measured wire bytes rather
than estimating usage from codec bitrate alone.

### DESK-01 Persist identity and validate native desktop behaviour

**Outcome:** Restarting Veil keeps a user's verified identity and setup on every desktop OS.
**Status:** Partial; Windows persistence exists. **Target:** 0.4.0. **Depends on:** BASE-01.

- Use macOS Keychain and an appropriate Linux secret store, plus native config directories.
  Handle locked/unavailable stores visibly without silently replacing a persisted identity.
- Verify restart, damaged storage, settings migration, verified contacts and node allow-lists.
- Test real camera, microphone, playback and screen-share delivery on all desktop platforms,
  including GNOME Wayland and KDE. Check actual DMG/AppImage installation and update paths.

### TRUST-01 Authenticate each forwarded media sender

**Outcome:** A malicious call member cannot place camera/audio in another person's tile or
advance that person's replay state with forged media.
**Status:** Known protection gap; see [Transparency](TRANSPARENCY.md#2-what-other-parties-can-see).
**Target:** 0.5.0; design starts alongside integration, fixes ship as soon as ready.
**Depends on:** Existing security and forwarded-media layers.

- Design sender authentication for camera/audio without trusting the forwarder or exposing
  content. Include shared-screen replay disruption in the review.
- Reject forged attribution before it can mutate the claimed sender's replay/decoder state.
- Add adversarial mesh/forwarder tests, removed-member/rekey tests and realistic cost
  measurements. Document protocol compatibility and update protection claims with the fix.

**Review questions:** Which data is signed/authenticated, what binds sender/room/epoch/frame,
and can an invalid frame change replay state before verification? Test forged counters,
reordered legitimate frames, cross-room/epoch reuse and intentionally malicious participants.
Do not treat a safety-code comparison as a substitute for media attribution.

### GROUP-01 Sustain calls beyond a small mesh

**Outcome:** Adding people or losing a forwarder does not silently strand the call.
**Status:** One-forwarder calls exist; capacity and failure handling are partial.
**Target:** 0.5.0. **Depends on:** DATA-01, TRUST-01, OPS-01.

- Validate five-person calls across real networks, including membership changes, screen shares
  and forwarding. Exercise a new guest unable to reach the active forwarder.
- Measure capacity instead of relying only on participant count. Keep session/control work
  responsive while routes dial, with bounded queues and explicit failure states.
- Implement recovery after an active forwarder outage, or a clear bounded failure/rejoin path.
  Test transitions, stale tickets and removal. Cascading is a later extension, not an exit gate.

**Current policy to revisit:** The actual implementation uses the host's successful preflight
and a strict majority of members, not unanimous agreement. A failed member can lose media;
an already forwarded call does not preflight every new joiner. Decide and test whether to use
mixed routes, an explicit refusal, or another bounded fallback. Do not describe majority
reachability as evidence that every participant can hear everyone.

### IOS-01 Bring the shared experience to iPhone

**Outcome:** An iPhone can join the same calls with native permissions and lifecycle behaviour.
**Status:** Not implemented. **Target:** Conditional 0.6.0. **Depends on:** MOB-01, MOB-02 and
access to a Mac with Xcode and an iPhone.

Implement the launcher, AVFoundation camera/audio, Keychain storage, invitation handling,
permissions and signed distribution. Repeat the mobile call, audio and interruption checks
on hardware. Reuse the call/security engine; do not create a separate protocol.

### READY-01 Establish a supported release

**Outcome:** Users know which experience is supported and how its security was assessed.
**Status:** Ongoing hardening; 1.0 gate not met. **Target:** 1.0.0.
**Depends on:** The mandatory capability set in the 0.1.14 through 0.5.0 milestone table.
IOS-01 is optional.

- Resolve known security weaknesses as they are found. Before 1.0, obtain an independent
  review of the supported protocol, identity/storage, native boundaries and update path;
  fix release-blocking findings and publish the review scope and remaining limitations.
- Define and pass a supported OS/device/network matrix, soak checks and regression suite.
- Verify signed distribution and updates on supported platforms, including OS signing and
  notarisation arrangements. Publish compatibility, security reporting and maintenance policy.
- Stabilise the documented SDK/provider surface, versioned events and local bridge, with a
  compatibility/deprecation policy and a reference embedding app tested against standalone Veil.

### SDK-01 Design the shared engine and provider contract

**Outcome:** An app author knows how to add Veil calling without copying its private app engine.
**Status:** Engineering design and source inventory recorded in
[ADR 0006](adr/0006-shared-engine-provider-contract.md) and the
[extraction inventory](ENGINE_PROVIDER_INVENTORY.md); implementation remains SDK-02.
**Target:** 0.1.14. **Depends on:** BASE-01.

Write an ADR identifying the engine/UI boundary, runtime ownership, storage and media adapters,
command/event model and supported host modes. Inventory which `veil-app::engine` dependencies
must move or become interfaces. Specify capture consent, session cancellation, event ordering,
backpressure and error taxonomy. Keep cryptographic state owned by the core.

**Complete when:** A reviewer can trace create/join/admit/mute/verify/reconnect/end through the
contract, and identify everything the host can access. The plan includes migration steps for
standalone desktop/Android and a non-GUI example. No new public API is called stable yet.

### SDK-02 Extract an engine that applications can consume

**Outcome:** A host can run a complete Veil call without launching Veil's window.
**Status:** Reusable protocol crates exist; complete headless product engine remains work.
**Target:** 0.3.0. **Depends on:** SDK-01, TRUST-02, TEST-01.

Implement the contract in a reusable Rust crate with injectable platform/storage/policy
adapters. Remove assumptions about global app settings, egui models, singleton devices and
the app updater. The SDK owns bounded workers, media sequencing, session secrets and teardown.

**Complete when:** A small non-GUI host can create/join, admit, exchange real audio/video and
leave with a standalone peer. Desktop and Android consume the shared engine path. Tests cover
cancellation at each lifecycle stage, slow event consumers, adapter failures and repeated
start/stop without leaked tasks, ports or capture. Document integration and licence obligations.

The consumer example must also build in a separate project outside the Veil workspace using
only documented public interfaces and reproducibly obtained pinned source/artifacts. Record
the dependency acquisition/build procedure; publication to crates.io is one distribution
option, not a prerequisite. Workspace path dependencies alone do not establish SDK usability.

### BIND-01 Provide a bounded local bridge for other languages

**Outcome:** A non-Rust host can use Veil without reimplementing the engine.
**Status:** Planned; select the initial host language/runtime before implementation.
**Target:** 0.4.0. **Depends on:** SDK-02, TRUST-02.

Prefer a versioned authenticated local process interface as the initial cross-language mode;
compare it with a native binding in the ADR. Define caller authorisation, message/frame limits,
session-scoped handles, media surfaces, event resync and crash cleanup. Do not expose a local
provider as an unauthenticated TCP service. A native FFI approach requires an explicit safety
design consistent with Veil's existing restriction on owned `unsafe` outside media boundaries.

**Complete when:** A non-Rust sample can complete a call, an unauthorised process is rejected,
malformed/oversized messages are bounded, host/provider crashes release capture, and schema
version mismatch gives a useful error. Explain process isolation's actual trust limits.

### EMBED-01 Prove Veil as another application's call provider

**Outcome:** A host such as Mailcraft offers calls that interoperate with standalone Veil.
**Status:** Core product requirement confirmed 23 November; no host integration implemented.
**Target:** 0.4.0. **Depends on:** SDK-02, BIND-01, ID-01, DIAG-01.

Choose an authorised reference host and implement provider selection, call launch/invitation,
lobby/admission, roster/video/audio, device controls, verification, reconnection and teardown.
Offer an optional supported call UI or a documented host-rendered path; neither requires
importing Veil's entire app. Map host account labels to display metadata unless device identity
binding is explicitly verified. Scope keys/settings to that host by default.

**Complete when:** Embedded and standalone users complete the same tested call journeys and
compatibility suite. Unloading/disabling the provider stops capture. The reference app needs
no mailbox/address-book export or central Veil account. Publish an adapter feasibility matrix
for Mailcraft and Discord/Slack-style extension, launch and gateway modes; verified permissions
determine which modes are actually offered for each host.

### LOCAL-01 Prove a useful call with WAN and DNS unavailable

**Outcome:** People on a reachable router/hotspot can talk when internet services fail.
**Status:** Local only, interface enumeration, signed full invites and discovery exist;
physical outage coverage remains open. **Target:** 0.2.0. **Depends on:** TRUST-02, DIAG-01.

Test Windows/Android and additional desktop devices on a router with WAN disconnected and
external DNS unavailable. Begin with two devices, then five to cross the forwarding threshold.
Join with full invitations and opt-in discovery; repeat with multicast blocked using manual
invites. Include a saved unreachable internet setup, no default route and an interface change.

**Complete when:** Admission, two-way real media, chat and membership changes work. A brief
participant interruption recovers within the defined window. Capture attributable Veil traffic
to verify no public helper, DNS, update, port-mapping or public-probe attempts. Record failures
caused by client isolation and explain them; Local only remains an on-link policy, not proof
of physical proximity or a guarantee about renamed virtual adapters.

### TRUST-02 Review every convenience path against the trust rules

**Outcome:** Easier discovery, joining and recovery do not make unauthorised entry easier.
**Status:** Existing controls and tests need an integrated adversarial review.
**Target:** 0.2.0; release-blocking findings are fixed immediately. **Depends on:** BASE-01.

Threat-model discovery broadcasts/code fetching, invitations, interface/address hints,
host-control resume, helper registration and forwarding transitions. Test spoofing, replay,
cross-room/epoch confusion, stale removed-member recovery, key substitution, floods, oversized
messages and worker/queue exhaustion. An address hint may describe its authenticated source;
it must not turn Veil into a dial/probe service for an arbitrary third-party target.

**Complete when:** Each attack surface has trust/size/time/rate limits, negative tests and a
review disposition. Verify no failed connection silently disables pinning, Local only or
relay-only privacy. Record exploitable findings privately through the security process; keep
public roadmap entries focused on status and protection boundaries.

### INVITE-01 Make local and embedded joining understandable

**Outcome:** A person can choose a nearby code, full invite or QR scan and reach the correct lobby.
**Status:** Full invites, QR display and nearby-code fetch exist; camera QR decoding is absent.
**Target:** 0.2.0. **Depends on:** LOCAL-01, TRUST-02.

Explain the difference between a nearby service, an unverified host and a verified contact.
Add bounded on-device QR decoding with explicit camera permission and no uploaded frames.
Keep full-text paste/share as the fallback. Show malformed, expired, consumed and wrong-version
invites clearly. Deep links reuse the existing app instance without creating duplicate calls.

**Complete when:** A tester can join from a locally displayed QR, copied full invite and nearby
code, including offline and denied-camera cases. A decoded invite still verifies the signature
and requires admission. No private invitation secret appears in analytics, crash text or a
public beacon. The future host adapter reuses validation, not a second invitation parser.

### DIAG-01 Give people useful local connection evidence

**Outcome:** A user can understand a failed or degraded call and choose what evidence to share.
**Status:** Logs, connection checks and route statistics exist; a unified call diagnosis remains.
**Target:** 0.2.0. **Depends on:** BASE-01.

Distinguish reachable helper, permission to host, invitation retrieval, admission, membership
and usable media. Show permitted route, reconnecting state, media delivery and a specific
failure action. Add an explicit redacted diagnostic export with a preview and selectable
address/device detail. Keep logging bounded and local.

**Complete when:** Wrong key, refused host, UDP block, no route, denied permission and missing
media each produce distinct useful states. Export tests ensure no private keys, MLS secrets,
reusable invites or conversation content; sensitive metadata is visible to the user before
export. Local-only diagnostics work without internet and do not create an upload service.

### TEST-01 Make completion evidence repeatable

**Outcome:** Maintainers can reproduce what a release actually supports.
**Status:** Unit/integration/synthetic and one hardware exchange exist; full scenario coverage does not.
**Target:** 0.2.0. **Depends on:** AND-02, DIAG-01.

Maintain a scenario matrix with exact builds, devices/OS, topology, network restrictions,
steps, expected result and measured outcome. Separate unit tests, synthetic loopbacks, real
capture/playback, human listening and user-reported checks. Add deterministic failure
injection for recovery/revocation and boundedness where practical.

**Complete when:** The core scenarios below have runnable procedures and a release evidence
ledger, with an explicit pass/fail/not-run state per scenario. Preserve TEST-5G-01 as a passed
user-reported connection. Hardware/network tests remain explicit actions; CI must not claim
carrier or audible-quality coverage from localhost tests. Never fabricate population success
rates from a handful of manual calls.

### NET-02 Fail over between genuinely independent chosen helpers

**Outcome:** One helper failing does not make a reachable call depend on that machine forever.
**Status:** Planned; multiple addresses for one helper identity are not independent helpers.
**Target:** 0.3.0. **Depends on:** TRUST-02, DIAG-01.

Design separate helper identities, room/permission registration, invitation references,
selection and bounded retry policy. Define what metadata each helper receives and avoid
registering everywhere by default. A replacement node must enforce its own host permission
and preserve member identity; losing a node is not a reason to invent a new room owner.

**Complete when:** Two helpers with distinct keys and failure domains pass registration,
join-time and mid-call outage tests. A stale/refusing/wrong-key helper cannot become trusted
through failover. Removing a configured helper stops subsequent use under documented rules.
Existing direct/local calls remain usable without contacting a replacement unnecessarily.

### NET-03 Upgrade routes without downgrading privacy

**Outcome:** A relayed call can move to a better permitted direct path without restarting.
**Status:** Planned; current links retain the path that won the initial connection race.
**Target:** 0.3.0. **Depends on:** TRUST-02, DIAG-01.

Design authenticated path validation and handover with bounded probes and overlap. Keep
working media until a replacement is validated; discard stale migration results when a session,
member or policy changes. Re-evaluate after interface changes with backoff rather than an
unbounded background scan.

**Complete when:** Direct-to-relay and relay-to-direct transitions are tested under loss and
membership changes without attribution/replay errors. Hide my address never attempts an
address-revealing direct path; Local only never probes a public one. Record connection time,
recovery gap and helper traffic before/after rather than assuming direct is always faster.

### OPS-01 Make user-run helpers practical and bounded

**Outcome:** A person can operate a node for chosen hosts without becoming an unlimited public service.
**Status:** Configurable roles, host allow-lists and resource limits exist; operations evidence is limited.
**Target:** 0.3.0. **Depends on:** DIAG-01, TRUST-02.

Document private key custody, host grants/revocation, listen versus public addresses, service
startup, firewall ports, updates and capacity limits. Expose local role health and resource
metrics without content or a company telemetry backend. Test fairness under many sessions
sharing a carrier NAT before changing address-block allocation.

**Complete when:** A clean machine can run a restricted node following the guide; allowed hosts
work, strangers are refused, and their guests still require host admission. Load tests record
bounded memory/queues and behaviour at capacity. Crash/restart and key/config errors have
clear recovery. Container packaging can follow this contract; it must not imply an operated
Veil service or unreviewed default public access.

### ID-01 Keep contacts and host-app identities locally controlled

**Outcome:** People recognise verified devices across runs and across deliberate app integrations.
**Status:** Verified-key lists exist; a complete local contact and multi-app identity model does not.
**Target:** 0.4.0. **Depends on:** DESK-01, SDK-01, TRUST-02.

Design local aliases, public keys, verification provenance, key-change/revocation states and
short-lived address hints. Default embedded stores to host-specific namespaces. Decide how
an explicitly opted-in shared identity is accessed without distributing key files or granting
all hosts blanket authority. Do not equate a host account ID with a verified Veil person.

**Complete when:** Restart, key replacement, duplicate names, store lockout, separate host apps,
user deletion and consent-based export tests preserve the documented identity model. There
is no central contact lookup or automatic address-book import. Encryption/backup recovery
and deletion limits are documented before any history/contact export feature ships.

### MEDIA-01 Prioritise conversation and measure capacity

**Outcome:** Video does not make speech or call control unusable, and topology matches real resources.
**Status:** Media queues, Opus FEC/jitter handling and a capacity planner exist; app capacities are assumed.
**Target:** 0.5.0. **Depends on:** DATA-01, DIAG-01, TRUST-01.

Measure rate, loss, delay, queue age, encoder/decoder cost and memory. Preserve control/audio
priority across mesh, forwarder and TCP paths. Feed capacity estimates into topology/quality
planning with conservative bounds; do not interpret one transient estimate as permission for
an unlimited upload. Test slow receiver backpressure and repeated layer changes.

**Complete when:** Published throttling/loss and five-person runs show bounded latency/queues,
intelligible audio, effective receive subscriptions and stable adaptation. Record phone thermal,
battery and CPU observations before enabling a more expensive codec/encoder path. Optimise
copies/scaling/encoding only against a measured bottleneck and preserve safety checks.

### HOST-01 Transfer coordination without sharing private keys

**Outcome:** An explicitly authorised co-host can continue an eligible group call.
**Status:** Not implemented; current host departure ends the call.
**Target:** Unassigned extension after core reliability. **Depends on:** GROUP-01, TRUST-01, ID-01.

Start with planned handover, current roster consent/authority and an explicit epoch/ownership
transition. Unexpected host failure and partitions need a separate authority/election design.
No implementation may copy the host's private identity key or restore a removed participant.

**Design gate:** Specify who may commit MLS changes after transfer, how stale hosts are fenced,
how conflicting coordinators resolve, and what users see when continuity is impossible.
**Completion gate:** Adversarial partition/rejoin/removal tests and real planned/unplanned
outage evidence meet that design. Until then, document the ended-call behaviour honestly.

### NEAR-01 Explore nearby connectivity beyond an existing LAN

**Outcome:** Nearby devices may establish a usable local path without an existing router.
**Status:** Exploration; ordinary LAN/hotspot calling remains the supported foundation.
**Target:** Unassigned extension. **Depends on:** LOCAL-01, TRUST-02, MOB-01.

Evaluate Wi-Fi Aware/Direct and available platform equivalents on actual supported hardware;
Bluetooth may assist discovery/small exchanges rather than carrying video by assumption.
Document OS permissions, background behaviour and interoperability limits. Discovery and
physical transport must stay separate from identity/admission.

**Design gate:** A hardware feasibility matrix and measured two-device path identify what is
actually portable. **Completion gate:** Opt-in discovery, bounded energy/use, authenticated
joining and usable media pass adversarial/hardware checks. Geographic multi-hop community
networking requires its own routing, power and hardware project; this capability does not
promise to create a mesh or extend a radio's range.

### MSG-01 Explore local history and delayed private messages

**Outcome:** An optional message can wait on a user's device until a permitted path returns.
**Status:** Future extension; current in-call chat is not a persistent offline messenger.
**Target:** Unassigned extension. **Depends on:** ID-01, TRUST-02, TEST-01.

First design encrypted local history, retention, deletion, backups, multi-device key custody
and delivery receipts. Treat an optional user-owned mailbox as an additional consented role
with expiry/storage limits. Forwarding through other people adds its own abuse/metadata model;
do not turn call helpers into hidden archives.

**Design gate:** Explain sender/recipient authentication, deduplication, expiry, quotas,
compromise/recovery and what a mailbox learns. **Completion gate:** Delayed delivery,
replay/duplication, device loss and deletion tests support the published guarantees. Live
calls still need a continuous physical path; queued messages solve a different problem.

### SCALE-01 Extend capacity through willing peers and cascades

**Outcome:** People can contribute bounded forwarding capacity without a central provider.
**Status:** Planner supports broader layouts; transport uses one forwarder per room today.
**Target:** Unassigned extension. **Depends on:** GROUP-01, MEDIA-01, OPS-01, TRUST-01.

Deliver opt-in participant forwarding first, with measured upload, battery, thermal and
stop conditions. Then design forwarder-to-forwarder links, routing loops, partial reachability,
roster authorisation, resource isolation and regional placement. Keep forwarding blind to
content and explicit about observable metadata.

**Complete when:** Increasing group size has measured bandwidth/CPU/latency results; overload
or withdrawal of one participant's contribution cannot exhaust another's device or bypass
roster checks. Cascade failure tests verify bounded queues and recovery. No advertised room
size or throughput exceeds the tested supported configuration.

### PERF-01 Pursue 4K60 and efficient native media against measurements

**Outcome:** Capable devices get better video/screen quality without sacrificing conversation or power.
**Status:** Architectural ambition; current desktop camera is capped at 1080p and share at 1920 long-edge.
**Target:** Unassigned performance extension. **Depends on:** MEDIA-01, DATA-01, TEST-01.

Benchmark hardware H.264, GPU scale/decode, content-aware screens and candidate codecs against
current software paths. Review licence/patent/distribution constraints and native attack surface.
Choose supported resolution/frame-rate combinations by device capacity and actual connection,
not a universal marketing switch. Keep low-data/audio operation first-class.

**Complete when:** Reproducible hardware tests record capture-to-render delay, quality, wire
rate, CPU/GPU, memory and phone power/thermal behaviour. Pause/resume/layer transitions remain
correct. A 4K60 claim names the tested platform/hardware/path; 1.0 is not blocked on it.

### ORG-01 Add organisation adapters without changing the trust model

**Outcome:** Organisations add identity, policy and capacity to the same Veil engine/protocol.
**Status:** Architecture direction; enterprise adapters are not implemented.
**Target:** Unassigned extension beyond the core baseline. **Depends on:** SDK-02, EMBED-01, ID-01, OPS-01.

Define short-lived identity attestations bound to device keys and signed organisation policy;
keep keys generated and held on endpoints. Organisation-provided rendezvous/relay/forwarder
capacity remains replaceable. Add federation, visible recording or gateways only as separately
reviewed adapters, with explicit participant-visible plaintext access and policy conflicts.

**Complete when:** Personal and organisation-enabled clients interoperate using the supported
SDK/protocol. Invalid/expired/revoked policy/attestations fail safely. No invisible recorder,
administrator decryption key or automatic company-hosted social graph is introduced. An
organisation's opted-in data policy must be visible, not represented as Veil's default privacy model.

## Release gates and deliverables

Every milestone has a small set of reviewable deliverables. A successful demo advances the
evidence; it does not silently close storage, revocation, lifecycle or integration requirements.

| Target | Required deliverables | Explicit release blocker |
|---|---|---|
| 0.1.14 | Android integration reviews, pinned build instructions/CI, existing desktop checks, engine/provider ADR and API inventory | Unexplained native lifetime ownership, unreproducible build or accidental desktop regression |
| 0.2.0 | Offline hardware report, integrated convenience-path review, mobile/audio scenarios, QR/manual join checks, redacted diagnostics, signed preview/upgrade record | Removed-member recovery, weakened pinning/privacy, capture left running after leave or core offline call depending on WAN |
| 0.3.0 | Consumable headless Rust sample, desktop/Android engine migration, full TCP joining path, independent helper tests, measured receive controls and node load limits | GUI dependency in the SDK, UDP-only joining on the TCP path, unbounded retry/queue behaviour or privacy-leaking route upgrade |
| 0.4.0 | Native persistence matrix, host-scoped identity design, versioned authorised local bridge, reference embedded app and standalone interop report | Silent identity regeneration, unauthorised local IPC, plugin unload leaving capture alive or required central host data upload |
| 0.5.0 | Sender-attribution/replay review, measured group media, forwarding partial-failure decision, active-node outage results | Another member accepted as a different media sender, hidden participants without media or unbounded group resource use |
| 1.0.0 | Defined supported scope, independent review/findings disposition, API/protocol compatibility policy, full mandatory matrix, signed distribution and maintenance process | Any unresolved issue classified as release-blocking in the reviewed threat model or a protection claim unsupported on a supported path |

Before a release candidate, classify findings by exploitability, exposure and user impact,
record remediation or a justified scoped exclusion, and keep security reports private as
required. A known high-impact trust bypass is not made acceptable by labelling the package
beta. If a platform/path is excluded, the product and documentation must make that boundary
visible and fail safely rather than quietly offering the unsupported behaviour.

**Earlier group previews:** TRUST-01's allocation to 0.5 is a completion deadline, not permission
to defer a release-blocking attribution flaw. Before any earlier preview including forwarding,
classify the known malicious-member weakness and either fix it or enforce a documented scope
restriction where it is release-blocking. A five-device engineering test may exercise that
path with cooperating test participants; it does not establish safe use with untrusted members.
The 0.2 supported experience must name this boundary, including any disabled/group-limited path.
If no safe enforceable scope is accepted, TRUST-01 moves onto that release's critical path.

## Validation scenarios and evidence

The table is a required validation plan, not a claim that every test has run. Completed
evidence remains in the current-evidence ledger; release candidates attach exact builds and
reports to the scenarios that apply to their supported scope.

| Scenario | Setup and disturbance | Required observation | Current evidence |
|---|---|---|---|
| Local two-device | WAN/DNS unavailable; full invite; Windows and Android | Admit, two-way real media/chat; no attributable internet attempts | Local synthetic/hardware interoperability exists; full outage proof still open |
| Local five-device | Same outage, enough devices to exercise forwarding; one briefly disconnects | All admitted devices have visible usable paths; bounded safe recovery | Planned physical test |
| No multicast/no default route | Block discovery; keep usable on-link addresses; unreachable saved node | Manual joining works; optional internet configuration does not block local mode | Implementation exists; scenario evidence needed |
| 5G phone to desktop host | Phone on carrier data, desktop hosts | Successful join; capture route/build/media/duration on repeat | TEST-5G-01 passed as a user-reported connection |
| Two difficult external networks | Both peers behind restrictive NAT; chosen permitted node | Actual direct/punched/relay outcome and time to live | Planned path-separated tests |
| UDP blocked | Drop UDP; use permitted TCP/TLS helper | Fetch invite, join lobby, admit and exchange media without hidden UDP dependency | NET-01 not built |
| Helper failure | Two distinct helper keys; stop selected helper during join and call | Bounded failover, no trust relaxation, no unnecessary helper use for existing local paths | NET-02 not built |
| Privacy route migration | Enable Hide my address; make direct path available; change interfaces | No address-revealing direct attempt; policy remains enforced | Required NET-03 regression gate |
| Network transition | Wi-Fi to 5G and back, brief loss and address updates | Record outage/recovery time, epochs, roster and actual returned media | Recovery implementation partial; mobile scenario open |
| Remove during outage | Interrupt a member, remove it, restore network and replay old state | No resumed membership or new content access | Existing checks need integrated adversarial evidence |
| Mobile background/audio | Screen lock, background/foreground, Bluetooth disconnect, denied permissions | Speech/route/capture/service states match the user-visible outcome | Local media exchange passed; sustained listening/lifecycle open |
| Low-bandwidth media | Sustained throttling/loss with mesh, forwarder and TCP | Audio/control remain usable; camera receive limits reduce actual wire payload | Data modes exist; complete receive controls missing |
| Host/forwarder loss | Stop host; separately stop active forwarder; add unreachable guest | Honest terminal/recovery state, no hidden media exclusion | Host handover absent; forwarder recovery incomplete |
| Malicious forwarded sender | Forge attribution/counters, replay previous room/epoch and send invalid frames | Authentication before replay/decoder mutation; legitimate sender continues | TRUST-01 protection gap remains |
| Identity restart/failure | Restart each OS; lock/corrupt store; compare before/after | Stable identity or explicit safe failure; no silent replacement | Windows and Android storage foundations exist; Linux/macOS incomplete |
| SDK lifecycle | Non-GUI sample, cancel startup, slow event consumer, repeat create/leave | Complete interop, bounded events, no orphan resources or copied secrets | SDK product surface not yet built |
| Embedded host and standalone | Start in reference host, join from Veil; unload plugin mid-call | Same trust/permission flows; capture/tasks end correctly | EMBED-01 not built |
| Local bridge hostile client | Wrong caller, malformed/oversized data, stale handles, version mismatch | Rejected/bounded requests; no cross-app session or capture authority | BIND-01 not built |
| Upgrade/installation | Signed same-package upgrade; standalone and embedded compatible versions | Keys/contacts retained; incompatible API/wire versions fail clearly | Desktop packaging exists; Android signed upgrade/SDK matrix open |

### Measurements that inform decisions

| Measurement | Record | Decision it supports |
|---|---|---|
| Joining | Attempt count, successful path, time to lobby/admission and first usable media | Dial timing and whether additional helpers actually improve reach |
| Recovery | Physical loss duration, time after usable connectivity, roster/epoch changes and media gap | Supported interruption window and path migration policy |
| Conversation quality | Both-end listening verdict, echo/double-talk/dropout, device route | Audio processing/device changes before video optimisation |
| Bandwidth | Wire bytes by audio/camera/share/control, per route and policy | Receive controls, traffic priorities, topology and node budgets |
| Resources | CPU/GPU, queue age/bytes, memory, worker/task counts and shutdown time | Boundedness and where native/hardware optimisation is justified |
| Mobile cost | Device/OS, call duration, battery delta and thermal observations | Foreground/background limits, forwarding opt-in and default quality |
| Integration quality | Public versus private API use, host dependencies, cancellation/crash leaks | Whether the SDK is reusable rather than a wrapper around the full app |
| Privacy | Attributable destinations, beacon fields, helper metadata and exported logs | Whether selected privacy modes and data-minimisation rules hold |

Do not set a global call-success percentage from one device pair. Establish repeatable
baselines across the defined scenarios, then set performance thresholds against those
conditions. The proposed 10-second interruption, 20-second recovery and 30-minute soak gates
are starting acceptance targets; changing them requires a documented supported-behaviour
decision and updated tests, not quietly lowering the bar after a failed run.

## Open decisions and engineering risks

| Decision or risk | Why it matters | Required next evidence or decision |
|---|---|---|
| SDK process boundary | Native embedding is efficient but trusts the host process; IPC adds isolation and complexity | SDK-01 ADR with a minimal Rust host, media surface and lifecycle contract |
| Optional UI reuse | A toolkit-specific screen is not a universal embeddable widget | Choose launch-window versus host-rendered versus reusable component modes for the reference host |
| Cross-app identity | Automatic key sharing expands impersonation and compromise scope | Scoped default stores; explicit shared-identity consent/key-custody design |
| Platform integration permission | A named app may allow launching links but not replacing native calls | Check current documented APIs and authorisation before promising that adapter |
| Transport replacement | A new networking library can add hidden public discovery/default services | Compare in isolation behind the role interface, inspect defaults and capture actual traffic |
| Helper independence | Several addresses can still be one operator, one key or one failure domain | Distinct-key registration/failover design and practical outage test |
| Forwarder majority versus everyone | Current majority transition may strand a minority; new guests may lack a path | Decide explicit refusal, mixed routes or bounded fallback; make actual reach visible |
| Host continuity | MLS authority and partitions make handover a protocol change | Planned handover before unexpected failure/election; fence old authority |
| Media attribution cost | Added authentication must resist malicious members without overwhelming devices | Adversarial tests plus encode/verify bandwidth/CPU measurements |
| Nearby transport portability | Hardware/OS support and background permissions vary | Real platform feasibility matrix; keep LAN/manual invite fallback |
| Carrier NAT fairness | One address block may represent many legitimate users; device keys are cheap to mint | Loaded-relay experiments before changing abuse controls |
| Stored messages/history | New persistence and mailbox roles expand compromise, deletion and abuse scope | Separate opt-in storage/backup/retention threat model before implementation |
| Enterprise adapters | Attestation, policy, recording and gateways can change who is trusted | Explicit role design and participant-visible policy/plaintext access |

No open library or framework choice is a reason to discard the working call stack without
evidence. Prefer a small isolated prototype, an interoperability comparison and a rollback
plan. Keep the product goal fixed while allowing the implementation behind a role to improve.

## Next implementation slices

These are work packages, not calendar promises or authorization to implement all features now.

1. **Capture the foundation:** review the uncommitted Android extraction/platform work, make
   clean-checkout builds reproducible, and retain the LAN and user-reported 5G evidence.
2. **Define the shared provider:** SDK-01 ADR and contract inventory; trace a Mailcraft-style
   host journey without assuming access to that repository or a platform-specific plugin API.
3. **Defend the new exposure:** TRUST-02 review of discovery, invites, address hints and
   recovery. Begin the forwarded-media attribution design immediately; ship fixes when ready.
4. **Prove the mission on hardware:** two-device then five-device WAN/DNS-off calls, manual
   fallback with multicast blocked, removal during interruption and redacted diagnostics.
5. **Make phones dependable:** sustained listening/call/background runs, device-route changes
   and network transitions; collect failures before choosing hardware media optimisation.
6. **Extract and demonstrate:** headless SDK, standalone migration and cross-language provider
   sample. Add a reference host integration only after its trust/lifecycle contract is reviewable.

## Version and compatibility rules

- Continue the existing 0.1.x preview line for consolidation/fixes; 0.1.14 is an integration
  target, not a stable mobile promise. After 0.2.0, use patch releases for compatible fixes
  and minor releases for capability milestones or deliberate pre-1.0 compatibility breaks.
- Give testers prereleases such as `0.2.0-alpha.1` and `0.2.0-beta.1`. The current
  `0.1.13-mobile.3` label is a development preview, not a separate protocol generation.
- Record wire, invitation, node and persisted-data compatibility separately in every release.
  Matching version numbers alone are not evidence that two builds interoperate. A wire break
  must name the minimum compatible clients/nodes and whether fresh invitations are required.
- At 1.0, establish the supported public protocol/API surface. Compatible fixes use patch
  versions, compatible additions use minor versions, and incompatible changes use major versions.
- Track the Rust SDK, IPC/binding schema, host adapter and wire protocol explicitly. Product
  milestones do not promise those interfaces share one version or compatibility window.
  Publish tested combinations and a deprecation policy; never hide a protocol break inside
  a supposedly compatible host-plugin update.
- Never move a published tag. Allocate actual release numbers only after checking existing
  tags/releases. This roadmap does not change Cargo or Android versions.

## Work after the core milestones

QR camera scanning is now in the proposed 0.2 INVITE-01 scope. Android screen sharing remains
a separately scoped platform extension. IOS-01 is conditional on hardware/tooling; HOST-01,
NEAR-01, MSG-01, SCALE-01, PERF-01 and ORG-01 have design gates but no reserved version.
Allocate their release numbers after feasibility and scope decisions instead of implying a
fixed serial path to 0.7/0.8/0.9. Their absence does not remove the requirement for a reusable
engine/provider in the core baseline.

## Maintaining the roadmap

Every Veil session begins by opening the live roadmap and reading its highlighted next step.
Follow [the session workflow](../AGENTS.md) and [tracking guide](ROADMAP_TRACKING.md). Record
meaningful work transitions in `roadmap-state.json` with `tools/roadmap.py`; the live preview
polls that recorded state and refreshes document changes. A file-based HTML copy is a saved
snapshot, clearly distinguished from the live local view.

Use the capability ID in an issue or PR and link it here when work starts. Mark a capability
complete only when its exit evidence is recorded, with the build/commit, platform, network,
result and remaining limits. A test report should also distinguish user-reported results from
automated checks and identify direct/punched/relayed/forwarded paths when known. Do not record
private keys or reusable invitation secrets.

When scope changes, update the milestone and dependency together. Keep completed test evidence
instead of turning it back into a task. Move shipped behaviour to the changelog and refresh
the handoff; keep capability IDs stable so older discussions remain meaningful.
