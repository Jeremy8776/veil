//! Telling you when a newer Veil is out, and installing it when you ask.
//!
//! Purpose: once per start, ask GitHub for Veil's latest published release and, if it is
//! newer than this build, show "Update available" on the home screen. Where this copy can
//! update itself (`apply.rs`: the Windows MSI, the macOS app, the Linux AppImage) and the
//! build carries Veil's release key (`verify.rs`), "Install" downloads it, checks the
//! release key's signature and the file's SHA-256, and installs it. Otherwise the link opens
//! the release page and you install it yourself.
//!
//! Invariants:
//! - The check itself downloads and runs nothing, so a forged answer can at worst show a
//!   wrong version number. Installing is a separate click, and it trusts only the release
//!   key's signature, never the answer: the files are named and linked from the checked
//!   version alone (`download.rs`).
//! - One request: `GET` [`API`] over HTTPS, with the User-Agent `veil-app/<version>` and
//!   nothing else about you. GitHub sees your IP address, as with any website.
//! - The link is built here from a checked tag (`vX.Y.Z`), never taken from the answer.
//! - Drafts and pre-releases are ignored. The answer is read up to [`MAX_BODY`] bytes, and
//!   the request gives up after [`TIMEOUT`].
//! - Any failure (offline, the repository still private, a bad answer) shows nothing.
//! - Turned off, no request is made at all. The choice is kept as the file [`OFF_FILE`] in
//!   Veil's folder (`keystore::default_dir`).

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;

mod apply;
mod download;
mod verify;

/// The repository releases come from.
pub const REPO: &str = "Jeremy8776/veil";
const API: &str = "https://api.github.com/repos/Jeremy8776/veil/releases/latest";
/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Present in Veil's folder when you turned the check off.
const OFF_FILE: &str = "no-update-check";
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// GitHub's answer for one release is a few kilobytes, more with long notes.
pub const MAX_BODY: u64 = 256 * 1024;

/// A release version, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Reads `1.2.3`: three numbers of plain digits, nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut next = || {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// Reads a release tag, `v1.2.3`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }

    /// This build's version.
    pub fn current() -> Option<Self> {
        Self::parse(VERSION)
    }

    /// The release page for this version, built here, never read from GitHub's answer.
    #[cfg_attr(not(windows), allow(dead_code, reason = "only `open` reads it, Windows only"))]
    pub fn link(self) -> String {
        format!("https://github.com/{REPO}/releases/tag/v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The fields Veil reads from GitHub's release.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// From GitHub's answer, a published release newer than `current`, if there is one.
pub fn newer(body: &str, current: Version) -> Option<Version> {
    let release: Release = serde_json::from_str(body).ok()?;
    if release.draft || release.prerelease {
        return None;
    }
    Version::from_tag(&release.tag_name).filter(|v| *v > current)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// You turned the check off.
    Off,
    Checking,
    UpToDate,
    Available(Version),
    /// The check failed. Shown as nothing.
    Unknown,
}

/// Installing an update: what it is doing, or why it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Idle,
    /// A short line for the step under way.
    Working(String),
    /// The install has started; Veil should close.
    Started,
    Failed(String),
}

pub struct Updates {
    dir: Option<PathBuf>,
    status: Arc<Mutex<Status>>,
    /// A check ran or is running this start.
    started: bool,
    install: Arc<Mutex<Install>>,
    kind: apply::Kind,
}

impl Updates {
    /// Reads your choice from `dir` and, unless you turned the check off, starts it in the
    /// background.
    pub fn start(dir: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let off = dir.as_ref().is_some_and(|d| d.join(OFF_FILE).exists());
        let mut updates = Self::idle(dir);
        if !off {
            updates.check(ctx);
        }
        updates
    }

    fn idle(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            status: Arc::new(Mutex::new(Status::Off)),
            started: false,
            install: Arc::new(Mutex::new(Install::Idle)),
            kind: apply::Kind::current(),
        }
    }

    /// Whether this copy can install an update itself: it was installed from a package
    /// Veil knows how to replace, and the build carries Veil's release key.
    pub fn can_install(&self) -> bool {
        verify::release_key().is_some() && self.kind.asset(Version(0, 0, 0)).is_some()
    }

    pub fn install_state(&self) -> Install {
        self.install.lock().map_or(Install::Idle, |s| s.clone())
    }

    /// Downloads, checks and installs `version` in the background, unless an install is
    /// already running or has started.
    pub fn install(&self, version: Version, ctx: &egui::Context) {
        let Some(key) = verify::release_key() else { return };
        {
            let Ok(mut state) = self.install.lock() else { return };
            if matches!(*state, Install::Working(_) | Install::Started) {
                return;
            }
            *state = Install::Working(String::from("Starting"));
        }
        let (state, kind, ctx) = (self.install.clone(), self.kind.clone(), ctx.clone());
        let set = move |next: Install, state: &Mutex<Install>, ctx: &egui::Context| {
            if let Ok(mut s) = state.lock() {
                *s = next;
            }
            ctx.request_repaint();
        };
        let spawned = std::thread::Builder::new()
            .name(String::from("update install"))
            .spawn(move || {
                let mut report = |line: String| set(Install::Working(line), &state, &ctx);
                let result = apply::run(&kind, version, &key, &mut report);
                set(
                    match result {
                        Ok(()) => Install::Started,
                        Err(why) => Install::Failed(why),
                    },
                    &state,
                    &ctx,
                );
            });
        if spawned.is_err()
            && let Ok(mut s) = self.install.lock()
        {
            *s = Install::Failed(String::from("cannot start the download"));
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map_or(Status::Unknown, |s| *s)
    }

    pub fn enabled(&self) -> bool {
        self.status() != Status::Off
    }

    /// Turns the check on or off and remembers the choice. Turning it on checks now if this
    /// start has not already.
    pub fn set_enabled(&mut self, on: bool, ctx: &egui::Context) {
        if let Some(dir) = &self.dir {
            let marker = dir.join(OFF_FILE);
            // Best effort: if the folder cannot be written the choice lasts this run.
            let _ = if on {
                std::fs::remove_file(&marker)
            } else {
                std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&marker, b""))
            };
        }
        if on {
            if self.started {
                self.set(Status::Unknown);
                self.started = false;
            }
            self.check(ctx);
        } else {
            self.set(Status::Off);
        }
    }

    fn set(&self, status: Status) {
        if let Ok(mut s) = self.status.lock() {
            *s = status;
        }
    }

    fn check(&mut self, ctx: &egui::Context) {
        if self.started {
            return;
        }
        self.started = true;
        self.set(Status::Checking);
        let (status, ctx) = (self.status.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name(String::from("update check"))
            .spawn(move || {
                let found = match (fetch(), Version::current()) {
                    (Some(body), Some(current)) => match newer(&body, current) {
                        Some(version) => Status::Available(version),
                        None if serde_json::from_str::<Release>(&body).is_ok() => {
                            Status::UpToDate
                        }
                        None => Status::Unknown,
                    },
                    _ => Status::Unknown,
                };
                if let Ok(mut s) = status.lock() {
                    // You may have turned it off while the request ran.
                    if *s == Status::Checking {
                        *s = found;
                    }
                }
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.set(Status::Unknown);
        }
    }
}

/// The body of GitHub's answer, or `None` for any failure or a status other than 200.
fn fetch() -> Option<String> {
    fetch_from(API)
}

fn fetch_from(url: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .https_only(true)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("User-Agent", &format!("veil-app/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    if response.status() != 200 {
        return None;
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .ok()
}

/// Opens a release page in your browser.
#[cfg(windows)]
pub fn open(version: Version) {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let rundll = crate::firewall::system_program(
        std::env::var("SystemRoot").ok().as_deref(),
        "rundll32.exe",
    );
    // The link is built from three numbers, so it holds nothing a shell could read.
    let _ = std::process::Command::new(rundll)
        .args(["url.dll,FileProtocolHandler", &version.link()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

// shortcut: links open on Windows only. Use the platform's opener with the macOS and Linux
// back ends.
#[cfg(not(windows))]
pub fn open(_: Version) {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_three_plain_numbers() {
        assert_eq!(Version::parse("0.1.0"), Some(Version(0, 1, 0)));
        assert_eq!(Version::parse("10.20.30"), Some(Version(10, 20, 30)));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1..3", "+1.2.3", "1.2.3-rc1", " 1.2.3", "1.2.9999999999"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Version::from_tag("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::from_tag("1.2.3"), None, "tags start with v");
        assert!(Version::current().is_some(), "this build's version parses");
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert!(Version(0, 1, 1) > Version(0, 1, 0));
    }

    #[test]
    fn only_a_newer_published_release_is_offered() {
        let current = Version(0, 2, 0);
        let release = |tag: &str, draft: bool, pre: bool| {
            format!(r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{pre},"html_url":"https://evil.example"}}"#)
        };
        assert_eq!(newer(&release("v0.3.0", false, false), current), Some(Version(0, 3, 0)));
        assert_eq!(newer(&release("v0.2.0", false, false), current), None, "same version");
        assert_eq!(newer(&release("v0.1.9", false, false), current), None, "older");
        assert_eq!(newer(&release("v0.3.0", true, false), current), None, "draft");
        assert_eq!(newer(&release("v0.3.0", false, true), current), None, "pre-release");
        assert_eq!(newer(&release("v0.3.0-rc1", false, false), current), None, "not x.y.z");
        assert_eq!(newer("not json", current), None);
        assert_eq!(newer(r#"{"message":"Not Found"}"#, current), None);
    }

    #[test]
    fn the_link_is_built_from_the_version_alone() {
        assert_eq!(
            Version(1, 2, 3).link(),
            "https://github.com/Jeremy8776/veil/releases/tag/v1.2.3"
        );
    }

    #[test]
    fn turned_off_makes_no_request_and_is_remembered() {
        let dir = std::env::temp_dir().join(format!("veil-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        let mut updates = Updates::idle(Some(dir.clone()));
        updates.set_enabled(false, &ctx);
        assert!(dir.join(OFF_FILE).exists());
        let again = Updates::start(Some(dir.clone()), &ctx);
        assert_eq!(again.status(), Status::Off, "no check was started");
        assert!(!again.started);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asks GitHub for real: `cargo test -j 8 -p veil-app update -- --ignored --nocapture`.
    /// A public repository proves TLS and parsing; Veil's own prints `None` while private.
    #[test]
    #[ignore = "makes network requests to api.github.com"]
    fn fetch_reads_the_latest_release() {
        let public = "https://api.github.com/repos/cli/cli/releases/latest";
        let body = fetch_from(public).unwrap();
        assert!(newer(&body, Version(0, 0, 0)).is_some(), "cli/cli has a vX.Y.Z release");
        println!("Veil: {:?}", fetch().map(|body| newer(&body, Version(0, 0, 0))));
    }
}
