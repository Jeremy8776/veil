# Android integration review — 23 November 2026

AND-02 review of the Android candidate, initially `0.1.13-mobile.3` and now `0.1.13-mobile.4`.
This is an engineering review and
build record, not a mobile security audit or a claim that every lifecycle scenario passes.

## Source and integration boundary

Primary checkout: `C:\Users\jerem\Documents\ChatGPT\Veil\main`. Initial review used branch
`codex/release-0.1.13`, base `c032a89`. Integration now uses
`codex/android-preview-integration`, fast-forwarded to main's `264640f` (the same source tree)
before committing the reviewed increments. The older `source/` checkout was not synchronised
or edited. Remote CI results are recorded as they become available below.

The existing desktop main function now delegates to the `veil-app` library. The Android
`cdylib` delegates to `run_android`; both use the app's existing private engine and protocol
crates. This is application reuse, not an implemented headless provider. SDK-01's separate
design is [ADR 0006](adr/0006-shared-engine-provider-contract.md); extraction remains SDK-02.
Desktop eframe/X11/Wayland/accessibility and xcap dependencies remain target-gated; Android
uses NativeActivity, Camera2, AAudio, OpenH264 and the same Rust call/security stack.

## Native ownership and findings

| Boundary | Inspected behaviour | Disposition / remaining evidence |
|---|---|---|
| JNI Activity reference | `ffi/android.rs::init` obtains an independently owned global reference before the event loop. Each call clones the bridge outside its mutex and attaches the current thread. Local JNI references stay within that attachment. | No borrowed JNI reference is retained on a worker. The static global retains the Activity until process death; this is not reusable in-process teardown for an SDK. |
| Camera resources | Java owns the HandlerThread, CameraDevice, capture session and ImageReader. Open/configure callbacks compare a generation; close advances it and clears the frame slot. Images close with try-with-resources. Rust receives a copied frame with a size cap and validates its NV12 layout. | **Fixed:** the image callback checked generation before copying but could publish after close/switch cleared the slot. Publication now rechecks generation and resumed state while holding the same monitor as close. Compiled successfully; real-device pause/flip stress remains open. |
| Activity teardown | Pause closes Java capture and releases multicast. Destroy closes capture, stops the service, quits the camera thread, calls NativeActivity teardown and kills this dedicated process. | Explicit standalone-preview limitation: process termination is not proof of graceful Rust task shutdown or persisted pending writes. Repeated destroy/relaunch, worker shutdown and identity retention need hardware evidence before broader delivery. Do not copy this process policy into an embedded provider. |
| Rust media teardown | Leave/Home detach media taps and stop camera/microphone workers; the shared Worker waits up to one second, then lets a stuck device thread finish independently. | Bounded UI wait, not a guarantee that capture stopped within one second. Stress leave/switch/background with device failures under MOB-01/MOB-02. |
| Permissions | Requests run on Android's UI thread. Camera and microphone permission bits gate pre-join capture independently; denied video need not prevent audio. Camera open also requires a resumed Activity. | Repeated denial, permission revocation and service-start timing around backgrounding remain device tests. No admission or identity check is bypassed by a permission grant. |
| Foreground call service | Explicit call startup posts a non-exported microphone/camera service with granted type bits. Leave/Home stop it. No service restart is requested. | Verify screen lock/background, notification denial, independent camera/audio denial and rapid leave/start. Service type changes and platform exceptions need targeted lifecycle evidence; compilation alone is insufficient. |
| Keystore | Java uses a private AES-GCM AndroidKeyStore key, random IV and separate authenticated labels for identity/trust records. Decryption without the alias fails; input key bytes are wiped in Java finally, and returned arrays are cleared through JNI. Backups are disabled. | Existing shared keystore recovery can set aside an unreadable record and generate a replacement with a visible warning. Test missing/corrupt alias, storage failure and same-signer upgrade; do not promise identity recovery. Java/VM copies are best-effort wiping. |
| Invitations and private data | The exported Activity accepts bounded `veil:` input; Rust parses it before joining, with existing signed invite and admission checks. Files/logs are app-private; Android disables desktop update requests. | Full convenience-path trust review remains TRUST-02. No invites, keys or conversation content are recorded in this review/build metadata. |
| Native-code inventory | The launcher uses one scoped `unsafe(no_mangle)` exception; raw JNI conversions are in `veil-media` with safety comments. | Updated TRANSPARENCY's exception/table to match the Android candidate; desktop app code retains its unsafe prohibition. |
| Build tool selection | Previous build script selected an arbitrary installed NDK and Gradle. | **Fixed:** exact tool pins, SHA-256-verified Gradle bootstrap, `--locked` Rust build, pinned Android CI artifact, Android Clippy, APK signature/alignment checks and build metadata. |
| Build environment | The previous restoration code passed null through a .NET string overload, leaving originally absent variables present but empty on this PowerShell runtime. | **Fixed:** remove originally absent environment entries explicitly; retain original values for present entries. |
| Preview signing | Debug signer belongs to each build machine. | Documented: CI/local artifacts may have different signers; uninstalling to switch deletes identity. Stable signing and upgrade evidence remain DIST-01. |

## Reviewable integration plan

Keep these as small increments on the same candidate base; do not publish the entire dirty
workspace as an unexplained snapshot or include ignored JNI libraries, build output,
`local.properties`, signing stores or credentials.

1. **Shared app entry and platform gates:** desktop `main` → library launcher; target-specific
   dependencies; Android launcher/JNI, Camera2, Keystore and mobile screen-sharing stub;
   native-code inventory. Require Windows tests/Clippy and Linux/macOS build/lint evidence.
2. **Android shell and touch experience:** Gradle project/manifest, permissions, service,
   icons, insets, compact screens, nearby/input/preferences adaptations and retained hardware
   reports. Include the camera publication fix; verify teardown and denial cases on hardware.
3. **Build repeatability and CI:** pinned `toolchain.json`, build/verification script,
   `.github/workflows/android.yml`, artifact metadata and installation/signing documentation.
   This job depends on the Android source in the preceding increments. Run against the full
   candidate before merging; do not merge a workspace member without its source files.

The roadmap tooling/docs are an independent work package. No product version/tag was changed
for this review. SDK-01 remains the next design slice after the integration gate is closed.

## Verification record

- `cargo test --workspace --locked`: **810 passed, 20 ignored, 0 failed** on Windows.
  The first sandboxed attempt failed the key-pinned local server check; the complete rerun
  with loopback access passed. Ignored hardware/synthetic tests were not run in this review.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: **passed** on Windows.
- `python tools/licences.py --check`: **passed**.
- `mobile/android/build.ps1 -Check`: **passed** with the pinned toolchain. Android ARM64
  Rust build and Clippy, Java/resources packaging, APK signature and 16 KiB ZIP alignment
  checks passed. Java/AGP deprecation notices remain; the build deliberately uses Gradle 8.13.
- Fresh candidate-source build: **passed**, including Android Clippy and APK checks. This uses a detached validation
  checkout at `main/target/and-02-source-check`, base `c032a89`, overlaid with the candidate's
  non-ignored source files. Its Rust target, JNI libraries and Gradle project output start
  empty; only installed toolchains and downloaded dependency caches are reused. This proves
  local source completeness, not a clean committed revision or remote CI success. The fresh
  build APK SHA-256 is `d4a62e8b1800f727403f71353cdd65f349c322d31f7f7c212669c4f9ae330b1b`.
  APK package inspection confirms `io.github.jeremy8776.veil.preview`, version code 15,
  `0.1.13-mobile.3`, minimum API 29 and target API 36. Builds are not claimed byte-identical.
- PowerShell build script syntax and Android workflow YAML: **parsed successfully**.
- Final `build.ps1 -Check` rerun: **passed**. A surrounding process-environment comparison
  confirmed that all eight scoped variables were restored exactly, including absent entries.

Native lifecycle disposition for AND-02: the dedicated-process ownership and one-second
worker wait/possible detached-thread limitation are explained above. This is a foreground engineering
preview; reliable background/locked-screen service operation and graceful reusable provider
teardown are not supported claims. Remaining device stress/denial/storage checks stay in
MOB-01/MOB-02/DIST-01 and the SDK-02 migration gates. A device was subsequently connected;
the bounded hardware observations below extend this review without claiming soak coverage.

**AND-02 closed on 25 November 2026.** The candidate was merged in
[PR #86](https://github.com/Jeremy8776/veil/pull/86) (`f2a01a3`). On `main` at `816b789`,
[CI](https://github.com/Jeremy8776/veil/actions/runs/45496676064) passed (Windows test and
Clippy, macOS 15 and Ubuntu 24.04 build and Clippy, supply chain), and
[Android preview](https://github.com/Jeremy8776/veil/actions/runs/45496676122) built the
`veil-android-arm64-preview` artifact from a fresh CI checkout. Lifecycle cases not covered
above stay in MOB-01, MOB-02, DIST-01 and SDK-02. The short solo-call test below exchanged no remote media.
The earlier LAN test and Jeremy's successful 5G report remain valid, separately scoped evidence.

## Connected-device and requested layout increment

Samsung SM-S9260, Android 17, connected by USB on 23 November. The candidate was installed
with the existing local debug signer. The protected device-identity file's digest remained
identical before/after replacement and all tested relaunches. No key bytes were copied or
published; there was no verified-contact file on this device to test.

- **Full portrait preview:** visually checked the entire 480×640 portrait camera frame in
  pre-join. UV cropping was replaced by aspect-preserving containment on desktop and mobile;
  captions occupy a separate footer. Existing encoder ladder sizing preserves source aspect.
- **Bento/join UI:** desktop's capture-free simulated call displayed five and then six
  participants after explicit demo admission; opening/closing People also reflowed the canvas.
  Shared geometry tests cover mixed portrait/landscape streams, tiny/phone/desktop areas and
  1–32 cells with no overlap/out-of-bounds. Motion tests verify new-member fade, keyed identity,
  mid-animation retarget continuity and animation-disabled snapping. This is not a real
  six-device media/capacity test.
- **Pre-join capture:** Android AppOps confirmed microphone capture was running, released
  after backgrounding, resumed in foreground, and closed on Veil Home. This exposed and fixed
  the old retained background pre-join microphone stream. Camera closes on background;
  video is restarted explicitly after returning.
- **Explicit solo call:** with a temporary Local only configuration and no remote guests,
  the call foreground service and microphone were active. After Home/background, camera
  capture stopped while the explicitly started call's microphone and foreground service
  remained active. Activity task removal stopped service and both capture sources.
- **Teardown/relaunch:** three additional task-removal/cold-launch cycles ended the old
  process and retained identity. The final Home screen rendered successfully.
- **Restoration:** the temporary-policy test's `finally` block stopped the test app, restored
  both original connection and preference files byte-for-byte, and verified identity retention.
  The original permissions, network and display settings were not changed.
- **Upgrade observation:** one no-focused-window ANR occurred after an earlier ADB package
  replacement; a stale Activity/task is suspected, not proven as its cause. Clean force-stop
  and relaunch recovered. The development `-Install` path now explicitly stops the old
  NativeActivity before replacement; subsequent install/relaunch and teardown cycles passed.
  This does not close the stable signed-upgrade or sustained-lifecycle gates.

Local full workspace validation after the main UI increment: **811 passed, 20 ignored**.
The final geometry refinement's targeted tests and desktop Clippy passed. Android build,
Clippy and APK checks passed. [CI 199](https://github.com/Jeremy8776/veil/actions/runs/45184913795)
and [Android preview 2](https://github.com/Jeremy8776/veil/actions/runs/45184913798) passed for
`74cbedc` before the requested bento/microphone increment. The final candidate's checks are
tracked on [PR #86](https://github.com/Jeremy8776/veil/pull/86); earlier green runs are not
substituted for checks on a changed head.

## Final review follow-up

The final permission-denial review identified an unnecessary service start for a receive-only
call. `veilCallActive` now starts the capture foreground service only when at least one
camera/microphone permission is granted; receive-only calls retain communication audio mode
without starting that service. The service's no-permission early stop remains defensive;
it must not attempt foreground promotion with an unpermitted capture type.

On the same Android 17 phone, a bounded test temporarily denied both permissions (including
user-fixed denial), started a Local only solo call, observed `call: Live`, and verified the
same process was alive after a further eight seconds. AppOps reported neither capture source
running, and no foreground capture service was present. The call screen rendered correctly.
The test's `finally` block restored both permission grants/flags and both original settings
files exactly, and verified the protected identity digest. This tests the new path; it does
not establish that the old service-stop path reproduced the review's predicted crash.

`build.ps1 -Check -Install` passed after the guard. Android Clippy now explicitly includes
`veil-android`, `veil-app` and `veil-media` as primary packages with warnings denied, rather
than relying on dependency compilation to lint the app/media crates.

Two older automated findings were checked against actual source and resolved without code
changes: pinned egui 0.37's `Id::unique(source)` hashes its source with fixed seeds (so the
leave-prompt and font-cache IDs are stable), and `docs/roadmap.html` is already a tracked
file in the PR. These were false positives, not defects hidden by a review dismissal.

## Remembered camera mirror control

Jeremy explicitly requested mirroring for **everyone's camera view**, rather than self-preview
only. The existing shared capture pipeline already mirrors NV12 before both the preview
conversion and the engine video tap. That behaviour is preserved; screen sharing has a separate
capture path. Android now exposes **Mirror my camera** before joining and in the call's More
menu, backed by the same `preferences.json` store as the desktop control. Help text states
that everyone sees the reflected camera picture and the choice is remembered.

On the connected Android 17 phone, the switch visibly reflected the whole portrait image.
After a force-stop/cold restart it remained selected, and a Local only solo call's More menu
showed the same selected state. The test restored both original settings files exactly and
verified identity retention. No new remote-media delivery was claimed from this solo check;
the shared NV12 mirror's existing padding/chroma and invalid-buffer tests both passed, and
all six preference tests passed. The Android build, expanded Clippy and same-signer install
also passed with the new control.

## Screen share ownership (SHARE-02)

| Boundary | Behaviour | Remaining evidence |
|---|---|---|
| Consent and service | Each share opens Android's MediaProjection prompt. On yes, `ShareService` enters the foreground with type `mediaProjection`, then obtains the projection. No yes, no service. | Phone test: start, denial, rapid start/stop. |
| Frame hand-off | `ScreenShare.java` copies each RGBA image into a new array with a size header, then publishes it only if its generation is current and the share live, under the same lock that stops it. Rust checks the Java array length before copying and validates the header and size cap (`projection.rs`). | Phone test of frame rate and memory use at the share size. |
| JNI references | All calls go through `ffi/android.rs`'s owned global Activity reference and current-thread attachment. No JNI reference is kept on the share or sound workers. | None beyond the existing bridge. |
| Teardown | Stop share, the notification's Stop sharing, Android's indicator (`MediaProjection.Callback.onStop`), screen off, leaving the call, task removal and Activity destruction release sound, display, reader, projection and service together. A second stop does nothing. | Phone test: each route ends in a clear state with capture stopped. |
