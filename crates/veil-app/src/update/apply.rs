//! Installing a newer Veil over this copy.
//!
//! Purpose: work out how this copy was installed ([`Kind`]), download the matching release
//! file, check it against the signed checksum list (`verify.rs`), and hand it over:
//!
//! | Installed as | Release file | Install |
//! |---|---|---|
//! | Windows MSI | `veil-<v>-windows-x64.msi` | a copy of Veil runs `msiexec /i` (Windows asks for permission) while Veil closes, waits, then starts Veil again |
//! | macOS `Veil.app` | `veil-<v>-macos-universal.dmg` | copy `Veil.app` from the disk image over this one, reopen it |
//! | Linux AppImage | `veil-<v>-linux-x64.AppImage` | rename it over this one, start it |
//! | Anything else (portable zip, deb, rpm, tar.gz, a build) | none | the release page only |
//!
//! Invariants:
//! - Nothing is installed unless the signature over the checksum list is Veil's release key
//!   and the file's SHA-256 matches its line.
//! - Programs are started by full path (`System32`, `/usr/bin`), never found on a search path.
//! - The person starts every install with a click; on Windows they also confirm Windows' own
//!   prompt, which names the publisher when the MSI is signed.

use std::path::{Path, PathBuf};

use super::Version;
use super::{download, verify};

/// The file an MSI install leaves beside `veil-app.exe` (`packaging/windows/veil.wxs`).
const MSI_MARKER: &str = "installed-by-msi";
/// The first argument that makes `veil-app` finish an MSI update instead of opening a window.
const FINISH_ARG: &str = "--finish-msi-update";
/// The copy of `veil-app.exe` that waits on Windows Installer, saved beside the MSI. The
/// installed one cannot wait: Windows Installer replaces it.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "MSI updates are Windows only")
)]
const HELPER: &str = "veil-update-helper.exe";

/// How this copy of Veil was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The Windows Installer package.
    Msi,
    /// The macOS app bundle at this path.
    MacApp(PathBuf),
    /// The Linux AppImage at this path.
    AppImage(PathBuf),
    /// Updated by hand: a portable zip, a package manager, a tar.gz or a development build.
    Manual,
}

impl Kind {
    /// This running copy's kind.
    pub fn current() -> Self {
        let Ok(exe) = std::env::current_exe() else {
            return Self::Manual;
        };
        detect(&exe)
    }

    /// The release file this kind installs from, if Veil can install it itself.
    pub fn asset(&self, version: Version) -> Option<String> {
        let x64 = cfg!(target_arch = "x86_64");
        match self {
            Self::Msi if x64 => Some(format!("veil-{version}-windows-x64.msi")),
            Self::MacApp(_) => Some(format!("veil-{version}-macos-universal.dmg")),
            Self::AppImage(_) if x64 => Some(format!("veil-{version}-linux-x64.AppImage")),
            _ => None,
        }
    }

    /// Where the release file `name` is saved before it is installed.
    fn download_path(&self, name: &str) -> Result<PathBuf, String> {
        let dir = match self {
            // Next to the AppImage, so the final rename stays on one file system.
            Self::AppImage(path) => {
                let dir = path.parent().ok_or("cannot find the AppImage's folder")?;
                return Ok(dir.join(format!(".{name}")));
            }
            Self::Msi => crate::keystore::default_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("updates"),
            _ => std::env::temp_dir().join(format!("veil-update-{}", std::process::id())),
        };
        // Earlier downloads are of no use now.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make a download folder ({e})"))?;
        Ok(dir.join(name))
    }

    fn install(&self, file: &Path) -> Result<(), String> {
        match self {
            Self::Msi => sys::msi(file),
            Self::MacApp(bundle) => sys::mac_app(bundle, file),
            Self::AppImage(target) => sys::appimage(target, file),
            Self::Manual => Err(String::from("this copy of Veil is updated by hand")),
        }
    }
}

/// The kind for a copy running from `exe`.
fn detect(exe: &Path) -> Kind {
    if cfg!(windows) {
        let marker = exe.parent().map(|dir| dir.join(MSI_MARKER));
        if marker.is_some_and(|m| m.is_file()) {
            return Kind::Msi;
        }
    } else if cfg!(target_os = "macos") {
        // A copy run from the disk image, or the private read-only copy macOS makes of an app
        // opened from Downloads (App Translocation), cannot be replaced.
        if let Some(bundle) = bundle_of(exe).filter(|b| !read_only_copy(b)) {
            return Kind::MacApp(bundle);
        }
    } else if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from)
        && path.is_absolute()
        && path.is_file()
    {
        return Kind::AppImage(path);
    }
    Kind::Manual
}

/// `Something.app` when `exe` is `Something.app/Contents/MacOS/<program>`.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == n);
    let is_app = bundle.extension().is_some_and(|e| e == "app");
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| bundle.to_path_buf())
}

/// A bundle macOS runs from a place Veil cannot write: Veil's mounted disk image
/// (`/Volumes/Veil`, or `/Volumes/Veil 1` when one is already mounted) or App Translocation.
fn read_only_copy(bundle: &Path) -> bool {
    let translocated = bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation");
    let disk_image = bundle.parent().is_some_and(|volume| {
        volume.parent() == Some(Path::new("/Volumes"))
            && volume
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("Veil"))
    });
    translocated || disk_image
}

/// The MSI and the installed `veil-app.exe` when `args` (without the program name) ask this
/// process to finish an MSI update. Both must be absolute paths with the right extension.
fn finish_args(args: &[std::ffi::OsString]) -> Option<(PathBuf, PathBuf)> {
    let [flag, msi, exe] = args else { return None };
    if flag != FINISH_ARG {
        return None;
    }
    let (msi, exe) = (PathBuf::from(msi), PathBuf::from(exe));
    let has = |p: &Path, ext: &str| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
    (msi.is_absolute() && exe.is_absolute() && has(&msi, "msi") && has(&exe, "exe"))
        .then_some((msi, exe))
}

/// When this process was started to finish an MSI update: runs Windows Installer, waits for
/// it, starts the installed Veil again, and returns `true` so `main` exits without a window.
pub fn finish_from_args() -> bool {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|a| a != FINISH_ARG) {
        return false;
    }
    if let Some((msi, exe)) = finish_args(&args) {
        sys::finish_msi(&msi, &exe);
    }
    true
}

/// Downloads, checks and installs `version`. `report` gets a short line for each step.
/// `Ok` means the install has started and Veil should close.
pub fn run(
    kind: &Kind,
    version: Version,
    key: &[u8; 32],
    report: &mut dyn FnMut(String),
) -> Result<(), String> {
    let name = kind.asset(version).ok_or("this copy of Veil is updated by hand")?;
    report(String::from("Checking the release"));
    let sums = download::small(&download::url(version, "SHA256SUMS.txt"))?;
    let signature = download::small(&download::url(version, "SHA256SUMS.txt.sig"))?;
    let digest = verify::signed_digest(&sums, &signature, key, &name)?;
    let dest = kind.download_path(&name)?;
    let mut last = None;
    download::file(&download::url(version, &name), &dest, &digest, &mut |done, total| {
        let text = match total {
            Some(total) if total > 0 => format!("Downloading {}%", done * 100 / total),
            _ => format!("Downloading {} MB", done / 1_000_000),
        };
        if last.as_ref() != Some(&text) {
            report(text.clone());
            last = Some(text);
        }
    })?;
    report(String::from("Installing"));
    kind.install(&dest)
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use std::path::Path;

    /// msiexec shows its own progress window; no console flashes up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Copies this `veil-app.exe` beside the MSI and starts the copy, which hands the MSI to
    /// Windows Installer and starts Veil again afterwards (`finish_msi`). Windows asks for
    /// permission (the package is for all users) and replaces Veil once Veil has closed.
    // shortcut: the checked MSI waits in your own folder until msiexec opens it, so a program
    // already running as you could swap it in that moment; Windows' prompt then names an
    // unknown publisher. Hold the file open until msiexec has it, or move to a service, if
    // that matters.
    pub fn msi(file: &Path) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find Veil ({e})"))?;
        let helper = file
            .parent()
            .ok_or("no download folder")?
            .join(super::HELPER);
        std::fs::copy(&exe, &helper).map_err(|e| format!("cannot prepare the install ({e})"))?;
        std::process::Command::new(&helper)
            .arg(super::FINISH_ARG)
            .arg(file)
            .arg(&exe)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(drop)
            .map_err(|e| format!("cannot start the install ({e})"))
    }

    /// Runs in the helper copy: installs the MSI, waits for Windows Installer to finish, then
    /// starts Veil again. Veil comes back even when the install was cancelled or failed, so
    /// you are never left without it.
    pub fn finish_msi(msi: &Path, exe: &Path) {
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
}

#[cfg(target_os = "macos")]
mod sys {
    use std::path::Path;
    use std::process::Command;

    pub fn msi(_: &Path) -> Result<(), String> {
        Err(String::from("not a Windows install"))
    }

    pub fn finish_msi(_: &Path, _: &Path) {}

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

    /// Copies `Veil.app` from the disk image beside `bundle`, swaps it in, and opens it. A
    /// running app keeps working when its bundle is renamed.
    pub fn mac_app(bundle: &Path, dmg: &Path) -> Result<(), String> {
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
        let swapped = swap(bundle, &mount.join("Veil.app"));
        let _ = run(
            "/usr/bin/hdiutil",
            &["detach".as_ref(), "-force".as_ref(), mount.as_os_str()],
        );
        let _ = std::fs::remove_dir_all(work);
        swapped?;
        Command::new("/usr/bin/open")
            .arg("-n")
            .arg(bundle)
            .spawn()
            .map(drop)
            .map_err(|e| format!("Veil is updated; open it again ({e})"))
    }

    fn swap(bundle: &Path, source: &Path) -> Result<(), String> {
        let parent = bundle.parent().ok_or("cannot find Veil's folder")?;
        let staged = parent.join(".Veil.app.update");
        let old = parent.join(".Veil.app.old");
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&old);
        // ditto keeps the code signature, symbolic links and permissions.
        run("/usr/bin/ditto", &[source.as_os_str(), staged.as_os_str()])
            .map_err(|e| format!("cannot write to {} ({e})", parent.display()))?;
        std::fs::rename(bundle, &old).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staged);
            // macOS 13 on blocks this unless Veil has App Management permission.
            format!(
                "cannot replace Veil ({e}). Allow Veil under System Settings > Privacy & \
                 Security > App Management, or install the new version by hand"
            )
        })?;
        if let Err(e) = std::fs::rename(&staged, bundle) {
            let _ = std::fs::rename(&old, bundle);
            return Err(format!("cannot replace Veil ({e})"));
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    }

    pub fn appimage(_: &Path, _: &Path) -> Result<(), String> {
        Err(String::from("not an AppImage"))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod sys {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub fn msi(_: &Path) -> Result<(), String> {
        Err(String::from("not a Windows install"))
    }

    pub fn mac_app(_: &Path, _: &Path) -> Result<(), String> {
        Err(String::from("not a macOS app"))
    }

    pub fn finish_msi(_: &Path, _: &Path) {}

    /// Renames the new AppImage over this one (the running copy keeps its open file) and
    /// starts it.
    pub fn appimage(target: &Path, file: &Path) -> Result<(), String> {
        let fail = |e: std::io::Error| {
            let _ = std::fs::remove_file(file);
            format!("cannot replace Veil ({e})")
        };
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).map_err(fail)?;
        std::fs::rename(file, target).map_err(fail)?;
        std::process::Command::new(target)
            .spawn()
            .map(drop)
            .map_err(|e| format!("Veil is updated; open it again ({e})"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_is_found_from_its_executable() {
        let exe = Path::new("/Applications/Veil.app/Contents/MacOS/veil-app");
        assert_eq!(bundle_of(exe), Some(PathBuf::from("/Applications/Veil.app")));
        for other in [
            "/usr/local/bin/veil-app",
            "/Applications/Veil/Contents/MacOS/veil-app",
            "/Applications/Veil.app/Contents/Resources/veil-app",
            "veil-app",
        ] {
            assert_eq!(bundle_of(Path::new(other)), None, "{other}");
        }
    }

    #[test]
    fn a_bundle_macos_runs_read_only_is_not_replaced() {
        for path in [
            "/Volumes/Veil/Veil.app",
            "/Volumes/Veil 1/Veil.app",
            "/private/var/folders/x/AppTranslocation/1234/d/Veil.app",
        ] {
            assert!(read_only_copy(Path::new(path)), "{path}");
        }
        for path in [
            "/Applications/Veil.app",
            "/Users/jo/Applications/Veil.app",
            "/Volumes/Apps/Veil.app",
        ] {
            assert!(!read_only_copy(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn only_a_complete_finish_request_is_read() {
        let args = |list: &[&str]| {
            list.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        let (msi, exe) = if cfg!(windows) {
            (
                r"C:\Users\jo\AppData\Local\Veil\updates\veil-1.2.3-windows-x64.msi",
                r"C:\Program Files\Veil\veil-app.exe",
            )
        } else {
            ("/tmp/veil-1.2.3-windows-x64.msi", "/opt/veil/veil-app.exe")
        };
        assert_eq!(
            finish_args(&args(&[FINISH_ARG, msi, exe])),
            Some((PathBuf::from(msi), PathBuf::from(exe)))
        );
        for bad in [
            args(&[FINISH_ARG, msi]),
            args(&[FINISH_ARG, msi, exe, "extra"]),
            args(&["--other", msi, exe]),
            args(&[FINISH_ARG, "update.msi", exe]),
            args(&[FINISH_ARG, exe, exe]),
            args(&[FINISH_ARG, msi, msi]),
        ] {
            assert_eq!(finish_args(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn each_kind_names_its_own_release_file() {
        let v = Version(1, 2, 3);
        let mac = Kind::MacApp(PathBuf::from("/Applications/Veil.app"));
        assert_eq!(mac.asset(v).unwrap(), "veil-1.2.3-macos-universal.dmg");
        assert_eq!(Kind::Manual.asset(v), None);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(Kind::Msi.asset(v).unwrap(), "veil-1.2.3-windows-x64.msi");
            let image = Kind::AppImage(PathBuf::from("/home/jo/Veil.AppImage"));
            assert_eq!(image.asset(v).unwrap(), "veil-1.2.3-linux-x64.AppImage");
        }
    }

    #[test]
    fn a_development_build_is_updated_by_hand() {
        let exe = std::env::current_exe().unwrap();
        if std::env::var_os("APPIMAGE").is_none() {
            assert_eq!(detect(&exe), Kind::Manual);
        }
    }
}
