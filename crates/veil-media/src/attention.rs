//! Getting your attention while Veil is in the background, on every platform.
//!
//! Purpose: one set of names for the app. On Windows these show a notification and flash the
//! taskbar button (`ffi/windows/attention.rs`); elsewhere they do nothing yet.
//!
//! Invariants:
//! - `window` is the main window's title; only this process's window of that name is touched.

/// Flashes the taskbar button of this process's window called `window` until it comes to the
/// front (`on`), or stops it.
#[cfg(windows)]
pub fn flash(window: &str, on: bool) {
    crate::ffi::windows::flash_taskbar(window, on);
}

/// Shows a notification. Clicking it brings this process's window called `window` forward.
#[cfg(windows)]
pub fn notify(window: &str, heading: &str, body: &str) -> Result<(), String> {
    crate::ffi::windows::show_notification(window, heading, body).map_err(|e| e.to_string())
}

/// Takes away the notifications [`notify`] showed.
#[cfg(windows)]
pub fn clear_notifications() {
    crate::ffi::windows::clear_notifications();
}

/// shortcut: no taskbar flash off Windows yet. Add each system's call (an urgency hint on
/// Linux, a Dock bounce on macOS) once hosting has been tested there.
#[cfg(not(windows))]
pub fn flash(_window: &str, _on: bool) {}

/// shortcut: no notification off Windows yet. Add each system's call once hosting has been
/// tested there.
#[cfg(not(windows))]
pub fn notify(_window: &str, _heading: &str, _body: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub fn clear_notifications() {}
