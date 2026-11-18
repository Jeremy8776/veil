//! Windows: installing the MSI (see `apply.rs`).
//!
//! Veil copies its own `veil-app.exe` beside the downloaded MSI and starts the copy with
//! [`super::FINISH_ARG`]. The copy waits for Veil to exit ([`super::start_after_exit`]),
//! hands the MSI to Windows Installer, waits for it, and starts the installed Veil again. The
//! installed `veil-app.exe` cannot do the waiting itself: Windows Installer replaces it.

use std::os::windows::process::CommandExt;
use std::path::Path;

/// msiexec shows its own progress window; no console flashes up.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts the helper copy. Windows asks for permission (the package is for all users) once
/// Veil has closed.
// shortcut: the checked MSI waits in your own folder until msiexec opens it, so a program
// already running as you could swap it in that moment; Windows' prompt then names an
// unknown publisher. Hold the file open until msiexec has it, or move to a service, if
// that matters.
pub fn msi(file: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find Veil ({e})"))?;
    let helper = file.parent().ok_or("no download folder")?.join(super::HELPER);
    std::fs::copy(&exe, &helper).map_err(|e| format!("cannot prepare the install ({e})"))?;
    super::start_after_exit(
        std::process::Command::new(&helper)
            .arg(super::FINISH_ARG)
            .arg(file)
            .arg(&exe)
            .creation_flags(CREATE_NO_WINDOW),
    )
    .map_err(|e| format!("cannot start the install ({e})"))
}

/// Runs in the helper copy: waits for Veil to exit, installs the MSI, waits for Windows
/// Installer, then starts Veil again. Veil comes back even when the install was cancelled or
/// failed, so you are never left without it.
pub fn finish_msi(msi: &Path, exe: &Path) {
    super::wait_for_exit();
    let msiexec = crate::firewall::system_program(
        std::env::var("SystemRoot").ok().as_deref(),
        "msiexec.exe",
    );
    let _ = std::process::Command::new(msiexec)
        .arg("/i")
        .arg(msi)
        .args(["/passive", "/norestart"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let _ = std::process::Command::new(exe).spawn();
}

pub fn mac_app(_: &Path, _: &Path) -> Result<(), String> {
    Err(String::from("not a macOS app"))
}

pub fn appimage(_: &Path, _: &Path) -> Result<(), String> {
    Err(String::from("not an AppImage"))
}
