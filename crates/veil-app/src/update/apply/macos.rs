//! macOS: replacing `Veil.app` (see `apply.rs`).
//!
//! While Veil is open, the disk image is mounted and its `Veil.app` copied beside this one as
//! `.Veil.app.update`, so a mount or copy failure shows in the app. Then a short `/bin/sh`
//! script waits for Veil to exit ([`super::start_after_exit`]), moves the old bundle aside,
//! moves the new one in (putting the old one back if that fails), and opens Veil again.
//!
//! Invariants:
//! - The script is a constant. Paths reach it only as positional arguments (`"$1"`), so no
//!   path is ever read as shell code.
//! - Veil is reopened whatever happens, the new copy or the old one put back. A failed swap
//!   adds a line to `veil.log`, which the reopened Veil keeps as `veil.previous.log`.

use std::path::Path;
use std::process::Command;

/// `$1` the bundle, `$2` the new copy, `$3` where the old one is moved, `$4` the log file.
const SWAP: &str = r#"/bin/cat >/dev/null
if /bin/mv "$1" "$3"; then
  if /bin/mv "$2" "$1"; then
    /bin/rm -rf "$3"
  else
    /bin/mv "$3" "$1"
    echo "update: could not move the new Veil.app into place; the old one is back" >>"$4"
  fi
else
  /bin/rm -rf "$2"
  echo "update: could not move Veil.app aside. Allow Veil under System Settings > Privacy & Security > App Management, or install the new version by hand" >>"$4"
fi
exec /usr/bin/open "$1"
"#;

pub fn msi(_: &Path) -> Result<(), String> {
    Err(String::from("not a Windows install"))
}

pub fn finish_msi(_: &Path, _: &Path) {}

pub fn appimage(_: &Path, _: &Path) -> Result<(), String> {
    Err(String::from("not an AppImage"))
}

/// Runs `program` and waits. A failure carries the last line it wrote to stderr.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot start {program} ({e})"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let why = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Err(format!("{program} failed ({}): {why}", output.status))
}

/// Copies `Veil.app` out of the disk image beside `bundle`, then starts the script that swaps
/// it in once Veil has closed.
pub fn mac_app(bundle: &Path, dmg: &Path) -> Result<(), String> {
    let parent = bundle.parent().ok_or("cannot find Veil's folder")?;
    let staged = parent.join(".Veil.app.update");
    let old = parent.join(".Veil.app.old");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    stage(dmg, &staged)?;
    let log = crate::log::folder().map_or_else(|| "/dev/null".into(), |d| d.join("veil.log"));
    super::start_after_exit(
        Command::new("/bin/sh")
            .args(["-c", SWAP, "veil-update"])
            .arg(bundle)
            .arg(&staged)
            .arg(&old)
            .arg(&log),
    )
    .map_err(|e| {
        let _ = std::fs::remove_dir_all(&staged);
        format!("cannot start the install ({e})")
    })
}

/// Mounts `dmg` and copies its `Veil.app` to `staged`, then unmounts it and deletes the
/// download.
fn stage(dmg: &Path, staged: &Path) -> Result<(), String> {
    let work = dmg.parent().ok_or("no download folder")?;
    let mount = work.join("mnt");
    std::fs::create_dir_all(&mount).map_err(|e| format!("cannot mount the update ({e})"))?;
    // The SHA-256 is already checked, so hdiutil's own checksum pass is skipped.
    run(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-noverify".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    // ditto keeps the code signature, symbolic links and permissions.
    let copied = run("/usr/bin/ditto", &[mount.join("Veil.app").as_os_str(), staged.as_os_str()]);
    let _ = run("/usr/bin/hdiutil", &["detach".as_ref(), "-force".as_ref(), mount.as_os_str()]);
    let _ = std::fs::remove_dir_all(work);
    copied.map_err(|e| {
        let _ = std::fs::remove_dir_all(staged);
        let folder = staged.parent().map_or_else(String::new, |p| p.display().to_string());
        format!("cannot write to {folder} ({e})")
    })
}
