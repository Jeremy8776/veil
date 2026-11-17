//! Embeds `macos/Info.plist` in the macOS executable.
//!
//! Purpose: macOS asks for camera and microphone permission only for programs that say why
//! (`NSCameraUsageDescription`, `NSMicrophoneUsageDescription`); without them it ends the
//! program on first use. A plain executable carries them in a `__TEXT,__info_plist` section,
//! the same place an app bundle's Info.plist would be read from.
//!
//! shortcut: a bare executable, not an app bundle. Build a signed `.app` with notarisation
//! before a wide release (see .github/workflows/release.yml).

fn main() {
    println!("cargo:rerun-if-changed=macos/Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let plist = std::path::Path::new(&dir).join("macos").join("Info.plist");
    println!(
        "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}
