# Veil mobile

Android is the first mobile target. It uses the same Rust call engine, signed invitations,
MLS membership, media encryption, QUIC transport, Opus framing and H.264 framing as desktop.
This is an installable development preview, not a store release.

Verified on 22 November 2026 with a Samsung SM-S9260 running Android 17: camera preview,
microphone capture, nearby-code invitation fetch, lobby admission, and a phone-to-desktop
encrypted call. The desktop decoded the phone's video and received its Opus packets while
sending synthetic video and silence back. The test host ended the call after its 15-second
media check. Desktop regression tests: 273 passed; desktop and Android Clippy checks passed.
This confirms local interoperability; listening quality and long background calls still
need separate checks.

On 23 November 2026, Jeremy reported successfully connecting his phone on 5G to his desktop
as the host. This establishes that internet connection scenario as user-tested. The report
does not specify the test date, builds, direct/punched/relayed route, duration or detailed
media checks. Network changes during a call and repeatable carrier/relay coverage remain
separate checks. See [the roadmap](../docs/ROADMAP.md) for version milestones and test evidence.

The `0.1.13-mobile.2` preview adds a compact Home screen, pinned pre-join and call actions,
larger touch controls, a video-first call stage, simpler connection settings and a remembered
display name. Hardware UI review covered portrait, landscape, Android font scale 1.3 and
chat with the keyboard open. System-bar, cutout and keyboard insets keep controls reachable;
the original device display settings were restored after checking. The app regression suite
remains at 273 passed, with desktop and Android Clippy checks passing.

The `0.1.13-mobile.3` launcher icon uses native adaptive vector layers: the Veil mark on
the mobile purple background, with padding inside Android's safe zone. Android 13 and later
also receive an opaque monochrome silhouette for themed launchers. The desktop logo is unchanged.

## Android build on Windows

`0.1.13-mobile.4` (version code 16) adds full portrait frames, a shared responsive bento call
canvas and smooth joins/reflows on desktop and phone. Captions sit below the whole camera
image. It also releases pre-join microphone capture while backgrounded; an explicitly started
call may keep audio with the visible foreground service. This does not establish long-call
or locked-screen reliability. The [integration review](../docs/ANDROID_INTEGRATION_REVIEW.md)
records the short hardware checks and their scope.

**Mirror my camera** is available before joining and in the call's More menu. It flips
both your own preview and the camera video others receive, keeps screen shares in their
normal orientation, and remembers the choice across app restarts.

Install JDK 17, Rust 1.98.1, and these exact Android SDK components. The pins live in
[`android/toolchain.json`](android/toolchain.json); AGP remains pinned to 8.11.0 in
`android/build.gradle`. This matches the [AGP compatibility requirements](https://developer.android.com/build/releases/agp-8-11-0-release-notes).
Run from this repository in PowerShell with Java 17 on `PATH` and `JAVA_HOME`:

```powershell
rustup toolchain install 1.98.1 --profile minimal --component clippy
rustup target add --toolchain 1.98.1 aarch64-linux-android
sdkmanager "platforms;android-36" "build-tools;36.0.0" "ndk;27.0.12077973"
.\mobile\android\build.ps1 -Check
# Optional, with an authorised phone connected:
.\mobile\android\build.ps1 -Install
```

The first build downloads Gradle **8.13** from its official distribution endpoint, checks
its pinned SHA-256 and extracts it under ignored `target/android-tools`. Internet is needed
for initial Rust/Gradle/Maven downloads; this is a build requirement, not a call requirement.
An existing Gradle 8.13 can instead be supplied with `-Gradle <path to gradle.bat>`.
Other Gradle versions are rejected. `-AndroidSdk` selects a non-default SDK location.

The script scopes its environment changes to the build, uses `Cargo.lock`, builds the Rust
library for ARM64, packages the matching NDK C++ runtime, and makes the debug APK:
`mobile/android/app/build/outputs/apk/debug/app-debug.apk`.
It verifies the APK's signature and 16 KiB ZIP alignment. `-Check` additionally runs Android
Clippy for the Android launcher, shared app and media libraries with warnings denied.
`build-info.json` beside the APK records the Git base revision,
whether the source was dirty, toolchain pins and APK SHA-256. A dirty build's revision alone
does not identify its changes; retain the candidate diff/review with the artifact.

The **Android preview** GitHub Actions workflow builds from a fresh checkout without a Rust
build cache and uploads the APK plus build info as `veil-android-arm64-preview` for 14 days.
It runs on pull requests, pushes to `main`, and manual dispatch. Existing Windows tests and
Linux/macOS checks remain separate required evidence. The integration is reviewed in
[PR #86](https://github.com/Jeremy8776/veil/pull/86); check its exact commit and workflow
results before treating an artifact as verified.

Debug APKs are signed with the build machine's development key. A CI artifact generally
cannot upgrade a locally signed preview; Android rejects the different signer. Keep using
the same local key to preserve app data. Uninstalling to switch signers deletes the device
identity and verified contacts. Stable signing and upgrade testing belong to DIST-01.

The debug package is `io.github.jeremy8776.veil.preview` and appears as **Veil Preview**.
It can coexist with a later production package. Android 10 or later is required.

## Native boundaries

`-Install` stops the previous NativeActivity before replacing the debug package and preserves
its data. One no-focused-window ANR occurred after an ADB replacement; a stale task is
suspected. Clean stop/launch and repeated task removal/relaunch passed. Signed production
upgrade behaviour remains a separate DIST-01 gate.

- `crates/veil-android` supplies the Android NativeActivity entry point.
- `crates/veil-app` exposes its existing application as a library. Desktop keeps its layout;
  Android uses compact Home, pre-join, call and connection layouts. The call state and engine
  are shared, including host admission, chat, safety numbers and device verification.
- `VeilActivity.java` owns Camera2, runtime permission prompts, clipboard access, local
  multicast access and Android Keystore. Camera frames are copied from strided YUV planes
  into rotated NV12. Rust encodes and decodes H.264 using the existing OpenH264 backend.
- Audio uses cpal's AAudio backend and the existing Rust Opus pipeline.
- JNI calls use an independently owned global Activity reference. They do not acquire
  AndroidApp's lifecycle lock while the event loop or capture workers are running.
- The UI has a single-task Activity, so launcher taps and invitation links reuse it.
  Destroying that Activity stops its camera and call service, completes NativeActivity's
  teardown and discards the app process. A later launch starts a fresh winit event loop.
- The device key and verified-key list are sealed using an Android Keystore AES-GCM key.
  Settings and diagnostic logs stay in the app's private files directory. App backups are off.
- An explicitly started call has an Android foreground-service notification. Leaving closes
  the call and stops that service. Backgrounding closes the camera; video can be restarted
  after returning. Reliable background calling and battery behavior still need soak tests.

## Networking

For a local call, both devices must be on a reachable local network. Enable **Local only**
and disable **Hide my address**. The host can enable **Discoverable nearby**; the guest enters
the host's code and still waits for admission. A full invite works independently of discovery.
The app can receive `veil:` links and shared plain-text invitations from Android.

For mobile-data calls, disable Local only. A reachable rendezvous/relay is generally needed
when the host is behind a carrier NAT. The existing transport is QUIC/UDP: this preview does
not add a TCP fallback, create a hotspot, build an offline radio mesh, or extend radio range.

## Real-device interoperability check

The ignored desktop test below advertises a temporary nearby call, auto-admits its first
guest on a **trusted test LAN**, sends synthetic video and silence, and checks that the
phone's video is decoded and its Opus packets are received. Auto-admission is test-only.

```powershell
$env:VEIL_MOBILE_TEST_WAIT_SECS = '600'
cargo test -p veil-app --lib android_device_exchanges -- --ignored --nocapture
```

Join from the phone using the printed nearby code. The default setup window is 180 seconds;
the environment variable can extend it up to 900. This test uses the real codec and encryption
pipeline, but does not replace subjective audio checks or a mobile-data/relay test.

To read the debug app's diagnostic file without exporting other app data:

```powershell
adb shell run-as io.github.jeremy8776.veil.preview cat files/logs/veil.log
```

## Remaining platform work

This package is a foreground development preview for engineering tests with cooperating
participants. Background/locked-screen speech and receive-only background operation are
experimental. Activity destruction ends the dedicated app process; relaunch starts a fresh
runtime. Do not rely on this preview for unattended background calls or untrusted forwarded
group calls. A distributable supported release must resolve the roadmap's lifecycle and
forwarded-sender attribution gates; the CI artifact is not that release.

The current integration review, native ownership findings and proposed PR split are in
[`docs/ANDROID_INTEGRATION_REVIEW.md`](../docs/ANDROID_INTEGRATION_REVIEW.md).

Android screen sharing is unavailable in this preview; it is planned as SHARE-02
([ADR 0007](../docs/adr/0007-share-sound-is-a-separate-opt-in-stream.md)). Camera video, incoming shared screens,
and calls use the existing media protocol. Hardware H.264 encoding, echo cancellation,
headset routing, reconnection across Wi-Fi/mobile transitions and long background calls need
dedicated mobile validation before release. Desktop update installation is disabled on Android.

iOS remains a later stage. The shared call/security engine and compact layouts are reusable;
an iOS launcher, AVFoundation/audio integration, Keychain storage, permissions, lifecycle and
signed builds still need implementation and a Mac with Xcode. There is no iPhone build yet.
