//! Build script: Windows version information for `veil-probe.exe`.

fn main() {
    windows_resources();
}

/// Windows only: version information (VERSIONINFO) in `veil-probe.exe`, which code signing checks
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
        .set("FileDescription", "Veil probe: test client")
        .set("CompanyName", "Veil")
        .set("LegalCopyright", "Copyright (c) the Veil authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "veil-probe.exe")
        .set("InternalName", "veil-probe");
    if let Err(e) = res.compile() {
        if std::env::var_os("VEIL_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows version information failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=veil-probe.exe built without version information: {e}");
    }
}
