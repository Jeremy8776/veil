//! Veil's log file, for people to send when something goes wrong.
//!
//! Purpose: release builds have no console, so what used to go to stderr went nowhere. Each
//! start writes `veil.log` in [`folder`]; the last run's log is kept as `veil.previous.log`,
//! because after a freeze or a crash the useful log is the one from before the restart.
//! Panics are written too. "Logs" on the home screen opens the folder.
//!
//! Invariants:
//! - The log stays on this device. Nothing reads it but the person who opens the folder.
//! - No names, chat, invite links or keys: lines carry states, timings and error text, and
//!   people are named by the first 8 hex digits of their device key at most.
//! - Logging never fails the app: a folder that cannot be written means stderr only.
//! - One file never passes [`MAX_BYTES`]; later lines are dropped with one note saying so.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "veil.log";
const PREVIOUS: &str = "veil.previous.log";
/// A day of heavy trouble fits; a loop writing errors cannot fill the disk.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

struct Sink {
    file: File,
    written: u64,
    full: bool,
}

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

/// Where the logs live: `%LOCALAPPDATA%\Veil\logs` on Windows, `~/Library/Logs/Veil` on
/// macOS, `$XDG_STATE_HOME/veil` (or `~/.local/state/veil`) elsewhere.
pub fn folder() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        var("LOCALAPPDATA").map(|base| base.join("Veil").join("logs"))
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library").join("Logs").join("Veil"))
    } else {
        var("XDG_STATE_HOME")
            .or_else(|| var("HOME").map(|home| home.join(".local").join("state")))
            .map(|base| base.join("veil"))
    }
}

/// Starts this run's log in `dir`, keeps the last run's, and logs panics. Call once, first.
pub fn start(dir: Option<&Path>) {
    let sink = dir.and_then(|dir| open(dir).ok());
    let _ = SINK.set(Mutex::new(sink));
    note(&format!(
        "Veil {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        note(&format!("panic: {info}"));
        previous(info);
    }));
}

fn open(dir: &Path) -> std::io::Result<Sink> {
    fs::create_dir_all(dir)?;
    let current = dir.join(FILE);
    if current.exists() {
        // Best effort: if the rename fails the new run appends to the old log instead.
        let _ = fs::rename(&current, dir.join(PREVIOUS));
    }
    let file = OpenOptions::new().create(true).append(true).open(&current)?;
    let written = file.metadata().map_or(0, |m| m.len());
    Ok(Sink { file, written, full: false })
}

/// Writes one line to the log and to stderr. Use the [`note!`](crate::note) macro.
pub fn note(text: &str) {
    let line = format!("{} {text}\n", clock(SystemTime::now()));
    eprint!("{line}");
    let Some(sink) = SINK.get() else { return };
    let mut guard = sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(sink) = guard.as_mut() else { return };
    if sink.full {
        return;
    }
    let bytes = line.as_bytes();
    if sink.written + bytes.len() as u64 > MAX_BYTES {
        sink.full = true;
        let _ = sink.file.write_all(b"(log full: later lines are not written)\n");
        return;
    }
    // Written straight through, so the line is on disk even if the app is then killed.
    if sink.file.write_all(bytes).is_ok() {
        sink.written += bytes.len() as u64;
    }
}

/// Logs the error that ended the app, then hands the result back unchanged. Without this, a
/// window that could not open (no graphics driver, display or font) reached stderr only.
pub fn record_exit<T, E: std::fmt::Display>(result: Result<T, E>) -> Result<T, E> {
    if let Err(why) = &result {
        note(&exit_line(why));
    }
    result
}

fn exit_line(why: &impl std::fmt::Display) -> String {
    format!("Veil stopped: {why}")
}

/// `2026-11-17 14:52:49.123Z`: UTC, so logs from people in other zones line up.
fn clock(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(i64::try_from(days).unwrap_or(0));
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// Days since 1970-01-01 to (year, month, day), Howard Hinnant's civil-from-days.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Opens the log folder in the file manager, creating it first.
pub fn open_folder() {
    let Some(dir) = folder() else { return };
    let _ = fs::create_dir_all(&dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// No console window flashes up.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let explorer = crate::firewall::system_program(
            std::env::var("SystemRoot").ok().as_deref(),
            "explorer.exe",
        );
        // The path is Veil's own folder, passed as one argument: no shell reads it.
        let _ = std::process::Command::new(explorer)
            .arg(&dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg(&dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
}

/// Writes a line to Veil's log file and stderr, like `eprintln!`.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {
        $crate::log::note(&format!($($arg)*))
    };
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn clock_reads_in_utc() {
        let at = UNIX_EPOCH + Duration::from_millis(1_795_445_569_123);
        assert_eq!(clock(at), "2026-11-23 14:52:49.123Z");
        assert_eq!(clock(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
        assert_eq!(civil(11_016), (2000, 2, 29), "leap day");
    }

    #[test]
    fn an_exit_error_is_named_and_handed_back() {
        assert_eq!(exit_line(&"no system UI font found"), "Veil stopped: no system UI font found");
        assert_eq!(record_exit(Err::<(), _>("no display")), Err("no display"));
        assert_eq!(record_exit(Ok::<_, &str>(7)), Ok(7));
    }

    #[test]
    fn a_new_run_keeps_the_last_runs_log() {
        let dir = std::env::temp_dir().join(format!("veil-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut first = open(&dir).unwrap();
        first.file.write_all(b"first run\n").unwrap();
        drop(first);
        let second = open(&dir).unwrap();
        assert_eq!(second.written, 0, "the new log starts empty");
        assert_eq!(fs::read_to_string(dir.join(PREVIOUS)).unwrap(), "first run\n");
        drop(second);
        let _ = fs::remove_dir_all(&dir);
    }
}
