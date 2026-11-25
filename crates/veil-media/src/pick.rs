//! Asking the person for one file, on every platform, without blocking the interface.
//!
//! Purpose: [`start`] opens the system's own file chooser: the Windows file dialog, the macOS
//! open panel, the desktop portal on Linux, or the Storage Access Framework picker on Android.
//! The interface calls [`poll`] each frame until it returns the [`Outcome`]. Used to import a
//! LUT (FX-01).
//!
//! Invariants:
//! - At most one chooser is open; [`start`] returns false while one is.
//! - A chosen file is read here, at most `max` + 1 bytes, so an oversized file is detected
//!   without reading all of it. The caller's parser decides what is too large.
//! - Paths and file contents are never logged.

use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

/// A chosen file: its name without the folder, and its first `max` + 1 bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Picked {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// How a chooser ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Picked(Picked),
    Cancelled,
    /// The chooser or the read failed. The text is plain enough to show.
    Failed(String),
}

#[derive(Default)]
struct State {
    open: bool,
    done: Option<Outcome>,
}

static STATE: Mutex<State> = Mutex::new(State {
    open: false,
    done: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn finish(outcome: Outcome) {
    let mut s = state();
    s.open = false;
    s.done = Some(outcome);
}

/// Reads up to `max` + 1 bytes of `path`, named by its file name.
pub fn read_capped(path: &Path, max: usize) -> Result<Picked, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| String::from("The file could not be read."))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Picked { name, bytes })
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn from_path(path: Option<std::path::PathBuf>, max: usize) -> Outcome {
    match path.map(|p| read_capped(&p, max)) {
        None => Outcome::Cancelled,
        Some(Ok(picked)) => Outcome::Picked(picked),
        Some(Err(why)) => Outcome::Failed(why),
    }
}

/// Opens a chooser for one file matching `pattern` (such as `*.cube`), described as `kind`.
/// Returns false if a chooser is already open.
pub fn start(title: &'static str, kind: &'static str, pattern: &'static str, max: usize) -> bool {
    {
        let mut s = state();
        if s.open {
            return false;
        }
        s.open = true;
        s.done = None;
    }
    open(title, kind, pattern, max);
    true
}

/// The outcome once the chooser has closed, once.
pub fn poll() -> Option<Outcome> {
    #[cfg(target_os = "android")]
    if state().open
        && let Some(result) = crate::ffi::android::take_picked()
    {
        finish(match result {
            Some((name, bytes)) => Outcome::Picked(Picked { name, bytes }),
            None => Outcome::Cancelled,
        });
    }
    state().done.take()
}

#[cfg(windows)]
fn open(title: &'static str, kind: &'static str, pattern: &'static str, max: usize) {
    spawn(
        move || match crate::ffi::windows::pick_file(title, kind, pattern) {
            Ok(path) => from_path(path, max),
            Err(_) => Outcome::Failed(String::from("The file chooser could not open.")),
        },
    );
}

#[cfg(target_os = "macos")]
fn open(title: &'static str, _kind: &'static str, _pattern: &'static str, max: usize) {
    crate::ffi::macos::pick_file(title, move |path| {
        // Read off the main thread, so a slow disk does not stall the window.
        spawn(move || from_path(path, max));
    });
}

#[cfg(target_os = "linux")]
fn open(title: &'static str, kind: &'static str, pattern: &'static str, max: usize) {
    spawn(move || match portal(title, kind, pattern) {
        Ok(path) => from_path(path, max),
        Err(()) => Outcome::Failed(String::from(
            "The file chooser could not open. Is xdg-desktop-portal installed?",
        )),
    });
}

#[cfg(target_os = "android")]
fn open(_title: &'static str, _kind: &'static str, _pattern: &'static str, max: usize) {
    if crate::ffi::android::pick_file(max).is_err() {
        finish(Outcome::Failed(String::from(
            "The file chooser could not open.",
        )));
    }
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "macos",
    target_os = "android"
)))]
fn open(_title: &'static str, _kind: &'static str, _pattern: &'static str, _max: usize) {
    finish(Outcome::Failed(String::from(
        "Choosing a file is not available here.",
    )));
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn spawn(work: impl FnOnce() -> Outcome + Send + 'static) {
    let started = std::thread::Builder::new()
        .name("veil-pick".into())
        .spawn(move || finish(work()));
    if started.is_err() {
        finish(Outcome::Failed(String::from(
            "The file chooser could not open.",
        )));
    }
}

/// The desktop portal's file chooser. `Ok(None)` when cancelled.
#[cfg(target_os = "linux")]
fn portal(title: &str, kind: &str, pattern: &str) -> Result<Option<std::path::PathBuf>, ()> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    pollster::block_on(async {
        let request = SelectedFiles::open_file()
            .title(title)
            .accept_label("Import")
            .modal(true)
            .multiple(false)
            .filter(FileFilter::new(kind).glob(pattern))
            .send()
            .await
            .map_err(|_| ())?;
        match request.response() {
            Ok(files) => Ok(files.uris().first().and_then(|u| file_path(u.as_str()))),
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
            Err(_) => Err(()),
        }
    })
}

/// The local path in a `file://` URI, percent-decoded. Anything else is `None`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn file_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len());
    let mut bytes = rest.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            let s = std::str::from_utf8(&hex).ok()?;
            out.push(u8::from_str_radix(s, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    let path = String::from_utf8(out).ok()?;
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode() {
        assert_eq!(
            file_path("file:///home/a/My%20Looks/Warm%2B.cube"),
            Some("/home/a/My Looks/Warm+.cube".into())
        );
        assert_eq!(file_path("file://localhost/x.cube"), Some("/x.cube".into()));
        assert_eq!(file_path("https://example.com/x.cube"), None);
        assert_eq!(file_path("file:///bad%2"), None);
        assert_eq!(file_path("file:///bad%zz"), None);
        assert_eq!(file_path("file://host/x.cube"), None);
    }

    #[test]
    fn reads_are_capped() {
        let path = std::env::temp_dir().join(format!("veil-pick-{}.cube", std::process::id()));
        std::fs::write(&path, [7u8; 100]).unwrap_or_default();
        let picked = read_capped(&path, 10);
        let _ = std::fs::remove_file(&path);
        let picked = picked.unwrap_or(Picked {
            name: String::new(),
            bytes: Vec::new(),
        });
        assert_eq!(picked.bytes.len(), 11, "max + 1, so oversize is detected");
        assert!(picked.name.starts_with("veil-pick-"));
        assert!(read_capped(Path::new("/no/such/file.cube"), 10).is_err());
    }
}
