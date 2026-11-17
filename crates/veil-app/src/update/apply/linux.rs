//! Linux: replacing the AppImage (see `apply.rs`).
//!
//! While Veil is open, the new AppImage is downloaded beside this one and made executable, so
//! those failures show in the app. Then a short `/bin/sh` script waits for Veil to exit
//! ([`super::start_after_exit`]), renames the new AppImage over this one and starts it.
//!
//! Invariants:
//! - The script is a constant. Paths reach it only as positional arguments (`"$1"`), so no
//!   path is ever read as shell code.
//! - Veil is started again whatever happens, the new copy or the old one. A failed rename
//!   adds a line to `veil.log`, which the restarted Veil keeps as `veil.previous.log`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// `$1` the new AppImage, `$2` this one, `$3` the log file.
const SWAP: &str = r#"/bin/cat >/dev/null
if ! /bin/mv -f "$1" "$2"; then
  /bin/rm -f "$1"
  echo "update: could not replace $2; this is still the old version" >>"$3"
fi
exec "$2"
"#;

pub fn msi(_: &Path) -> Result<(), String> {
    Err(String::from("not a Windows install"))
}

pub fn finish_msi(_: &Path, _: &Path) {}

pub fn mac_app(_: &Path, _: &Path) -> Result<(), String> {
    Err(String::from("not a macOS app"))
}

/// Makes the download executable, then starts the script that renames it over `target` and
/// starts it once Veil has closed.
pub fn appimage(target: &Path, file: &Path) -> Result<(), String> {
    let fail = |why: String| {
        let _ = std::fs::remove_file(file);
        why
    };
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| fail(format!("cannot replace Veil ({e})")))?;
    let log = crate::log::folder().map_or_else(|| "/dev/null".into(), |d| d.join("veil.log"));
    super::start_after_exit(
        std::process::Command::new("/bin/sh")
            .args(["-c", SWAP, "veil-update"])
            .arg(file)
            .arg(target)
            .arg(&log),
    )
    .map_err(|e| fail(format!("cannot start the install ({e})")))
}
