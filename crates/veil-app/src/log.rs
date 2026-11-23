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
    #[cfg(target_os = "android")]
    return crate::keystore::default_dir().map(|dir| dir.join("logs"));
    #[cfg(not(target_os = "android"))]
    folder_for(std::env::consts::OS, |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

#[cfg(any(not(target_os = "android"), test))]
fn folder_for(os: &str, var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    if os == "windows" {
        var("LOCALAPPDATA").map(|base| base.join("Veil").join("logs"))
    } else if os == "macos" {
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
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current)?;
    let written = file.metadata().map_or(0, |m| m.len());
    Ok(Sink {
        file,
        written,
        full: false,
    })
}

/// Writes one line to the log and to stderr. Use the [`note!`](crate::note) macro.
pub fn note(text: &str) {
    let line = format!("{} {text}\n", clock(SystemTime::now()));
    eprint!("{line}");
    let Some(sink) = SINK.get() else { return };
    let mut guard = sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(sink) = guard.as_mut() else { return };
    if sink.full {
        return;
    }
    let bytes = line.as_bytes();
    if sink.written + bytes.len() as u64 > MAX_BYTES {
        sink.full = true;
        let _ = sink
            .file
            .write_all(b"(log full: later lines are not written)\n");
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

/// Opens the log folder without blocking the UI. Unix children are waited and reaped.
#[derive(Default)]
pub struct FolderOpener {
    pending: Option<FolderRequest>,
}

struct FolderRequest {
    result: std::sync::mpsc::Receiver<Result<(), String>>,
    deadline: std::time::Instant,
}

impl FolderOpener {
    pub fn start(&mut self, ctx: &eframe::egui::Context) -> Result<(), String> {
        if self.pending.is_some() {
            return Ok(());
        }
        let dir = folder().ok_or("Cannot find this device's log folder.")?;
        #[cfg(windows)]
        {
            let _ = ctx;
            // Explorer may hand off to an existing process; its exit code is unreliable.
            spawn_folder(&dir).map(|_| ())
        }
        #[cfg(not(windows))]
        self.start_worker(ctx.clone(), move || {
            let child = spawn_folder(&dir)?;
            wait_for_opener(child, &dir)
        })
    }

    #[cfg(any(not(windows), test))]
    fn start_worker(
        &mut self,
        ctx: eframe::egui::Context,
        job: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        let (send, receive) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        std::thread::Builder::new()
            .name("open-log-folder".into())
            .spawn(move || {
                let result = job();
                if let Err(problem) = &result {
                    note(problem);
                }
                let _ = send.send(result);
                ctx.request_repaint();
            })
            .map_err(|why| format!("Cannot start log folder opener: {why}"))?;
        self.pending = Some(FolderRequest {
            result: receive,
            deadline: std::time::Instant::now() + TIMEOUT,
        });
        wake.request_repaint_after(TIMEOUT);
        Ok(())
    }

    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let pending = self.pending.as_ref()?;
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if std::time::Instant::now() < pending.deadline {
                    return None;
                }
                // Some openers remain alive while their file-manager window is open.
                // Release the UI request; its worker keeps reaping that process, and
                // its eventual result cannot affect a later retry's separate channel.
                Err(String::from(
                    "The log folder opener is still running. Check your file manager, or click Logs to try again.",
                ))
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(String::from(
                "The log folder opener stopped before reporting a result.",
            )),
        };
        self.pending = None;
        Some(result)
    }
}

fn spawn_folder(dir: &Path) -> Result<std::process::Child, String> {
    fs::create_dir_all(dir)
        .map_err(|why| format!("Cannot create log folder {}: {why}", dir.display()))?;
    let mut command = folder_command(
        std::env::consts::OS,
        std::env::var_os("SystemRoot").as_deref(),
        dir,
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashes up when Explorer starts.
        command.creation_flags(0x0800_0000);
    }
    let opened = command.spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = match opened {
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => std::process::Command::new("gio")
            .arg("open")
            .arg(dir)
            .spawn(),
        other => other,
    };
    opened.map_err(|why| format!("Cannot open log folder {}: {why}", dir.display()))
}

#[cfg(any(not(windows), test))]
fn wait_for_opener(mut child: std::process::Child, dir: &Path) -> Result<(), String> {
    let status = child
        .wait()
        .map_err(|why| format!("Cannot wait for log folder opener {}: {why}", dir.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Cannot open log folder {}: file manager exited with {status}.",
            dir.display()
        ))
    }
}

/// Each native opener receives the folder as one argument, without a shell.
fn folder_command(
    os: &str,
    system_root: Option<&std::ffi::OsStr>,
    dir: &Path,
) -> std::process::Command {
    let program = match os {
        "windows" => windows_explorer(system_root).into_os_string(),
        "macos" => std::ffi::OsString::from("/usr/bin/open"),
        _ => std::ffi::OsString::from("xdg-open"),
    };
    let mut command = std::process::Command::new(program);
    command.arg(dir);
    command
}

/// Explorer lives directly in the Windows folder, unlike tools in System32.
fn windows_explorer(system_root: Option<&std::ffi::OsStr>) -> PathBuf {
    PathBuf::from(
        system_root
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| std::ffi::OsStr::new(r"C:\Windows")),
    )
    .join("explorer.exe")
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
    fn opener_exit_status_is_checked_after_waiting_for_the_child() {
        for code in [0, 7] {
            #[cfg(windows)]
            let mut command = {
                use std::os::windows::process::CommandExt;
                let mut command = std::process::Command::new("cmd.exe");
                command.args(["/c", &format!("exit {code}")]);
                command.creation_flags(0x0800_0000);
                command
            };
            #[cfg(not(windows))]
            let mut command = {
                let mut command = std::process::Command::new("/bin/sh");
                command.args(["-c", &format!("exit {code}")]);
                command
            };
            let result = wait_for_opener(command.spawn().unwrap(), Path::new("test logs"));
            assert_eq!(result.is_ok(), code == 0);
            if let Err(problem) = result {
                assert!(problem.contains("test logs") && problem.contains("file manager exited"));
            }
        }
    }

    #[test]
    fn opener_error_arrives_asynchronously_for_the_ui() {
        let mut opener = FolderOpener::default();
        let (release, wait) = std::sync::mpsc::channel();
        opener
            .start_worker(eframe::egui::Context::default(), move || {
                wait.recv().unwrap();
                Err(String::from("file manager failed"))
            })
            .unwrap();
        assert!(opener.poll().is_none(), "waiting must not block the UI");
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(result) = opener.poll() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not report completion"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(result, Err(String::from("file manager failed")));
        assert!(opener.poll().is_none(), "the error is delivered once");
    }

    #[test]
    fn a_slow_opener_times_out_and_cannot_block_or_poison_a_retry() {
        let mut opener = FolderOpener::default();
        let (release, wait) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        opener
            .start_worker(eframe::egui::Context::default(), move || {
                wait.recv().unwrap();
                finished.send(()).unwrap();
                Err(String::from("old opener failed"))
            })
            .unwrap();
        assert!(opener.poll().is_none());
        opener.pending.as_mut().unwrap().deadline = std::time::Instant::now();
        assert!(
            opener
                .poll()
                .unwrap()
                .unwrap_err()
                .contains("still running")
        );
        assert!(
            opener.pending.is_none(),
            "a timed-out opener must allow retries"
        );

        opener
            .start_worker(eframe::egui::Context::default(), || Ok(()))
            .unwrap();
        release.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(result) = opener.poll() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "retry did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            result,
            Ok(()),
            "a late failure belongs only to the old request"
        );
    }

    #[test]
    fn log_folder_uses_the_native_user_location() {
        let base = PathBuf::from("test user");
        assert_eq!(
            folder_for("windows", |key| (key == "LOCALAPPDATA")
                .then(|| base.clone())),
            Some(base.join("Veil").join("logs")),
        );
        assert_eq!(
            folder_for("macos", |key| (key == "HOME").then(|| base.clone())),
            Some(base.join("Library").join("Logs").join("Veil")),
        );
        assert_eq!(
            folder_for("linux", |key| (key == "XDG_STATE_HOME")
                .then(|| base.clone())),
            Some(base.join("veil")),
        );
        assert_eq!(
            folder_for("linux", |key| (key == "HOME").then(|| base.clone())),
            Some(base.join(".local").join("state").join("veil")),
        );
        assert_eq!(folder_for("linux", |_| None), None);
    }

    #[test]
    fn native_folder_openers_keep_the_path_as_one_argument() {
        let dir = Path::new("test user's log folder");
        let root = std::ffi::OsStr::new(r"D:\Windows");
        for (os, expected) in [
            ("windows", PathBuf::from(root).join("explorer.exe")),
            ("macos", PathBuf::from("/usr/bin/open")),
            ("linux", PathBuf::from("xdg-open")),
        ] {
            let command = folder_command(os, Some(root), dir);
            assert_eq!(command.get_program(), expected.as_os_str());
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                vec![dir.as_os_str()]
            );
        }
    }

    #[test]
    fn explorer_falls_back_to_the_windows_folder_not_system32() {
        for root in [None, Some(std::ffi::OsStr::new(""))] {
            let command = folder_command("windows", root, Path::new("logs"));
            assert_eq!(
                command.get_program(),
                PathBuf::from(r"C:\Windows")
                    .join("explorer.exe")
                    .as_os_str()
            );
        }
    }

    #[test]
    fn clock_reads_in_utc() {
        let at = UNIX_EPOCH + Duration::from_millis(1_795_445_569_123);
        assert_eq!(clock(at), "2026-11-23 14:52:49.123Z");
        assert_eq!(clock(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
        assert_eq!(civil(11_016), (2000, 2, 29), "leap day");
    }

    #[test]
    fn an_exit_error_is_named_and_handed_back() {
        assert_eq!(
            exit_line(&"no system UI font found"),
            "Veil stopped: no system UI font found"
        );
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
        assert_eq!(
            fs::read_to_string(dir.join(PREVIOUS)).unwrap(),
            "first run\n"
        );
        drop(second);
        let _ = fs::remove_dir_all(&dir);
    }
}
