//! Embeds `macos/Info.plist` in the macOS executable, and version information in the
//! Windows one.
//!
//! Purpose: macOS asks for camera and microphone permission only for programs that say why
//! (`NSCameraUsageDescription`, `NSMicrophoneUsageDescription`); without them it ends the
//! program on first use. A plain executable carries them in a `__TEXT,__info_plist` section,
//! the same place an app bundle's Info.plist would be read from.
//!
//! Release builds also ship `Veil.app` (packaging/macos), whose own Info.plist carries the
//! same texts; this section keeps a bare executable (`cargo run`, the tools archive) working.

fn main() {
    windows_resources();
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

/// Windows only: version information (VERSIONINFO) in `veil-app.exe`, which code signing checks
/// (SignPath requires the product name and version on every signed file). Elsewhere nothing.
/// A missing resource compiler is a warning, unless `VEIL_REQUIRE_WINRES` is set, as the
/// release workflow does.
fn windows_resources() {
    println!("cargo:rerun-if-env-changed=VEIL_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Veil")
        .set("FileDescription", "Veil")
        .set("CompanyName", "Veil")
        .set("LegalCopyright", "Copyright (c) the Veil authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "veil-app.exe")
        .set("InternalName", "veil-app");
    if let Err(e) = res.compile() {
        if std::env::var_os("VEIL_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows version information failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=veil-app.exe built without version information: {e}");
    }
}
