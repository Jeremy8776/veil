# Android integration review — 23 November 2026

AND-02 review of the local `0.1.13-mobile.3` candidate. This is an engineering review and
build record, not a mobile security audit or a claim that every lifecycle scenario passes.

## Source and integration boundary

Primary checkout: `C:\Users\jerem\Documents\ChatGPT\Veil\main`. Initial review used branch
`codex/release-0.1.13`, base `c032a89`. Integration now uses
`codex/android-preview-integration`, fast-forwarded to main's `264640f` (the same source tree)
before committing the reviewed increments. The older `source/` checkout was not synchronised
or edited. Remote CI results are recorded as they become available below.

The existing desktop main function now delegates to the `veil-app` library. The Android
`cdylib` delegates to `run_android`; both use the app's existing private engine and protocol
crates. This is application reuse, not completion of SDK-01/SDK-02 or a headless provider.
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
worker wait/quarantine limitation are explained above. This is a foreground engineering
preview; reliable background/locked-screen service operation and graceful reusable provider
teardown are not supported claims. Remaining device stress/denial/storage checks stay in
MOB-01/MOB-02/DIST-01 and the SDK-02 migration gates. No connected Android device was available
in this milestone session. These findings are recorded limitations, not fabricated test passes.

AND-02 stays **in progress** until the committed candidate's Android and existing desktop CI
pass and the integration is complete. No new phone install or real call was performed here.
The earlier LAN test and Jeremy's successful 5G report remain valid, separately scoped evidence.
