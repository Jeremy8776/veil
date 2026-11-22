//! Installing a newer Veil over this copy.
//!
//! Purpose: work out how this copy was installed ([`Kind`]), download the matching release
//! file, check it against the signed checksum list (`verify.rs`), and hand it over:
//!
//! | Installed as | Release file | Ready while Veil is open | After Veil exits (`apply/`) |
//! |---|---|---|---|
//! | Windows MSI | `veil-<v>-windows-x64.msi` | a copy of `veil-app.exe` beside the MSI | `msiexec /i` (Windows asks for permission) |
//! | macOS `Veil.app` | `veil-<v>-macos-universal.dmg` | the new `Veil.app` copied beside this one | swap the bundles |
//! | Linux AppImage | `veil-<v>-linux-x64.AppImage` | the new AppImage beside this one | rename it over this one |
//! | Anything else (portable zip, deb, rpm, tar.gz, a build) | none | | the release page only |
//!
//! Every platform works the same way: everything that can fail is done while Veil is open, so
//! the failure shows in the app. A helper started with [`start_after_exit`] then waits until
//! Veil has exited, replaces it, and starts Veil again, the new copy or the old one when the
//! replace failed.
//!
//! Invariants:
//! - Nothing is installed unless the signature over the checksum list is Veil's release key
//!   and the file's SHA-256 matches its line.
//! - Programs are started by full path (`System32`, `/usr/bin`), never found on a search path.
//! - The person starts every install with a click; on Windows they also confirm Windows' own
//!   prompt, which names the publisher when the MSI is signed.

use std::path::{Path, PathBuf};

use super::{NetworkGate, Version};
use super::{download, verify};

#[cfg_attr(target_os = "macos", path = "apply/macos.rs")]
#[cfg_attr(windows, path = "apply/windows.rs")]
#[cfg_attr(not(any(windows, target_os = "macos")), path = "apply/linux.rs")]
mod sys;

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
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot make a download folder ({e})"))?;
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

/// Starts `command` with its stdin a pipe this process never closes. The program reads
/// end-of-file ([`wait_for_exit`], or `cat` in a shell script) only when the operating
/// system closes the pipe, which is when Veil has exited. No process ids, no timers.
fn start_after_exit(command: &mut std::process::Command) -> std::io::Result<()> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Leaked on purpose: Veil exiting closes it, and that is the signal.
    std::mem::forget(child.stdin.take());
    Ok(())
}

/// In a helper: returns once the Veil that started it has exited ([`start_after_exit`]).
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "the macOS and Linux helpers are scripts")
)]
fn wait_for_exit() {
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
}

/// Downloads, checks and installs `version`. `report` gets a short line for each step.
/// `Ok` means the install has started and Veil should close.
pub fn run(
    kind: &Kind,
    version: Version,
    key: &[u8; 32],
    network: &NetworkGate,
    report: &mut dyn FnMut(String),
) -> Result<(), String> {
    let name = kind
        .asset(version)
        .ok_or("this copy of Veil is updated by hand")?;
    report(String::from("Checking the release"));
    let sums = download::small(&download::url(version, "SHA256SUMS.txt"), network)?;
    let signature = download::small(&download::url(version, "SHA256SUMS.txt.sig"), network)?;
    let digest = verify::signed_digest(&sums, &signature, key, &name)?;
    let dest = kind.download_path(&name)?;
    let mut last = None;
    download::file(
        &download::url(version, &name),
        &dest,
        &digest,
        network,
        &mut |done, total| {
            let text = match total {
                Some(total) if total > 0 => format!("Downloading {}%", done * 100 / total),
                _ => format!("Downloading {} MB", done / 1_000_000),
            };
            if last.as_ref() != Some(&text) {
                report(text.clone());
                last = Some(text);
            }
        },
    )?;
    report(String::from("Installing"));
    network.check()?;
    kind.install(&dest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn a_started_worker_observes_the_new_policy_before_downloading() {
        let network = NetworkGate::new(true);
        let worker = network.clone();
        let mut steps = Vec::new();
        let result = run(
            &Kind::Msi,
            Version(99, 0, 0),
            &[0; 32],
            &worker,
            &mut |step| {
                steps.push(step);
                network.set(false);
            },
        );
        assert!(result.unwrap_err().contains("Local only"));
        assert_eq!(steps, [String::from("Checking the release")]);
    }

    #[test]
    fn a_bundle_is_found_from_its_executable() {
        let exe = Path::new("/Applications/Veil.app/Contents/MacOS/veil-app");
        assert_eq!(
            bundle_of(exe),
            Some(PathBuf::from("/Applications/Veil.app"))
        );
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
