# Engine/provider extraction inventory

SDK-01 source inspection, 23 November 2026, for
[ADR 0006](adr/0006-shared-engine-provider-contract.md). Paths are relative to the repository.
This is a migration map of existing code, not a list of available public SDK APIs.

## Current public and private surfaces

| Surface | Current status | Extraction decision |
|---|---|---|
| `veil-app::run`, `run_android` | Public application/UI launchers | Keep in app; never describe as headless provider |
| `veil-app::engine::{Engine, Role, Taps}` | Private module; start/sync/act/watch/stats/pictures/drop | Replace app-facing boundary with typed provider requests/events and awaitable shutdown |
| `veil-call::{HostHandle, MemberHandle, CallNode, MediaSession}` | Reusable low-level session/network components | Continue internally; lifecycle/media integration belongs in engine |
| `veil-call::EpochKeys`, security identity/frame types | Public low-level cryptographic building blocks | Keep owned by engine/core; do not re-export through the higher-level host facade |
| `veil-media` native codecs/capture/audio | Shared platform building blocks | Keep implementations; add consent-aware factories and generation-scoped leases |
| Node/helper role crates | Reusable protocol/helper libraries | Preserve roles/protocol; no mandatory operator or service |

## Complete engine module map

Every row below starts under `crates/veil-app/src/engine/`.

| Module | Current coupling / responsibility | Destination or interface |
|---|---|---|
| `mod.rs` | Own Tokio runtime; egui Context; model Action/Call/CallStatus; KeyStore, Worker, Playback and DataUseCell; unbounded UI command channel, bounded decoder keyframe feedback | `veil-engine` lifecycle/actor; toolkit-free handles/types and bounded queues; platform/storage adapters |
| `view.rs` | Shared view, received pictures/chat, stats and tiles; egui repaint; UI participant/lobby models, Rgba and app locking helper | Versioned engine snapshots, bounded events/media sinks; UI mapping and repaint stay in app |
| `host.rs` | Host actor, lobby decisions, local helper roles, invitations, planning; app Action/CallStatus and `nearby::advertise(Shared)` | Engine host coordinator with scoped discovery lease and typed membership/lobby requests |
| `host/forwarder_check.rs` | Forwarder readiness/proposal/activation through parent host types and Shared | Engine topology coordination; retain honest partial-reach reporting, future GROUP-01 decision |
| `guest.rs` | Invite fetch/resolve/join/recovery; app Action/CallStatus; private local candidate caps link-up holding at 64 events and 64 validated chat/reaction actions; UI command channel remains unbounded | Engine guest coordinator; cancellation at every await, bounded commands and deferred byte/age budgets |
| `session.rs` | Epoch keys, sealing/opening, subscriptions, app messages and media routing; DataUseCell, model types and Shared | Engine protocol/media coordinator; secrets stay private; bounded event/sink interfaces |
| `video_out.rs` | Encoders, shared media Worker/lock, DataUse and captured buffers | Engine encoder worker using bounded source lease and toolkit-free media policy |
| `video_in.rs` | Decode gate, frame parsing and conversion; Worker/Shared and app Rgba conversion | Engine decoder worker and immutable frame sinks; reusable conversion moves to media if retained |
| `audio_out.rs` | Jitter/decode/mix/playback; app Worker/meter, Playback/gain and Shared | Engine mixer/worker plus injected output-device/playback adapter; no singleton speaker |
| `plan.rs` | NetConfig parsing, DNS and addresses, topology; environment overrides and app logging | Engine validated immutable network policy; environment/settings translation stays in standalone adapter |
| `invites.rs` | Invite time/lifetime helpers and validation | Engine invitation policy with injected/testable clock where needed |
| `names.rs` | Name validation/announcements over sealed app messages | Engine protocol helper |
| `screen_proof.rs` | Signing/validating screen media attribution | Engine/protocol helper; no host-provided signing keys |
| `wanted.rs` | Tile width → forwarder demand | Engine demand types; UI alone translates render size to requested demand |
| `stats.rs` | Link/video sampling; UI LinkPath enum | Engine diagnostic snapshots and route types; app overlay formatting stays in app |
| `auto_step.rs` | Data-use adaptation; UI LinkPath enum | Engine bounded adaptive policy; preserve consent before any automatic camera disable |
| `router.rs` | Port mapping lease, Shared and UI RouterPort | Engine lease manager + structured connection events; explicit opt-in from session policy |
| `pinhole.rs` | IPv6 pinhole lease and app logging | Engine lease manager, scoped cancellation and diagnostics |
| `reach.rs` | Public reach check; UI Reachable and Shared | Engine permitted reach diagnostics; no probing that widens Local only/address-hiding policy |
| `loopback.rs` | Synthetic engine tests and ignored Android-device interoperability test; current UI/start plumbing | Retain app regression harness during migration; add engine fake-adapter lifecycle/interop tests separately |

## Adjacent responsibilities

| App dependency | Current behaviour | Required boundary |
|---|---|---|
| `app.rs`, `model.rs`, `screens/`, `mobile.rs`, `mobile/widgets.rs` | Screen state/actions, roster/chat rendering and permission/control gestures | App-owned presentation; request IDs plus generation-scoped provider handles replace UI indices |
| `layout.rs`, `layout/motion.rs`, `screens/bento.rs` | Shared portrait-aware bento cells, whole-image fitting and keyed join/reflow motion | Remain in the presentation layer; engine sees explicit final receive demand, not egui geometry/animation state |
| `keystore.rs`, `json_file.rs` | Protected key/verified-list files, persistence/recovery, config paths | Scoped engine identity/trust store; native protection adapters; explicit failure/reset instead of hidden replacement |
| `media.rs`, `media/mic.rs` | Capture workers, taps, blur/noise/gain and bounded UI wait on drop | Consent/source leases and joinable/quarantined workers; core owns media sequencing, host owns UI intent |
| `devices.rs`, `volume.rs` | Selected devices, gain, speaker route and atomics | Platform enumeration/device lease/output adapter; explicit requested/effective state |
| `convert.rs` | NV12 to Rgba conversion | Toolkit-free reusable conversion or media adapter; egui upload/texture lifetime stays app-owned |
| `data_use.rs` | DataUse and shared policy cell | Engine policy value type/effective capability result; UI selector remains app-owned |
| `connection.rs` | Saved setups, environment precedence and config validation | Standalone configuration adapter outputs validated policy; engine reads no global settings |
| `nearby.rs` | Both egui discovery UI and `advertise(Arc<engine::Shared>)` | Separate bounded discovery service/lease from app UI; no egui dependency in engine |
| `log.rs`, `lobby_alert.rs`, `veil-media::attention` | Global diagnostic sink, notifications and focus | Inject redacted diagnostic/attention events; permission-aware host notifications |
| `preferences.rs`, `theme.rs`, `system_fonts.rs`, `icons.rs`, `logo.rs` | App preferences, system fonts and drawing | Remain in app; pass explicit supported media preferences only |
| `firewall.rs`, `update/`, `share/` | Admin/network UI, updater, screen picker/preview/texture UI | Remain standalone services; explicit authorised platform operations, no engine updater |
| `veil-media/src/ffi/android.rs`, Java Activity/CallService | Static global Activity reference, Java camera/Keystore/service lifetime | Preserve preview implementation now; SDK-02 uses independently owned adapter handles and generations; standalone process policy stays in launcher |
| `veil-call` host/member event channels | Unbounded low-level notifications; recovery/route buffering | Bound at producer and intermediate stages during SDK-02, with explicit overload termination |

## Design review trace

Inspection covered every engine module above and the adjacent dependencies found through
imports, `crate::` calls and their types. The current `Engine::start` eagerly creates codec
workers, reads identity from an app KeyStore and passes egui into Shared; therefore a public
re-export alone cannot satisfy the non-GUI host requirement. `Engine::act(Verify)` writes
trust through UI IDs; it must become an exact device/challenge request. The current drop path
has timeout-based runtime shutdown and the shared Worker can detach a stuck thread; therefore
the contract reports incomplete cleanup rather than guaranteeing shutdown from Drop.

The ADR traces create/join/admit/mute/verify/reconnect/end, names host-visible data and
private cryptographic state, specifies bounded command/event completion semantics, and gives
desktop/Android plus external non-GUI migration gates. No new public API or stable protocol
version was created by writing this inventory.
