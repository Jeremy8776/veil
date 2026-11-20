//! Notifications that get your attention while Veil is in the background, under one set of
//! names on every platform.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | Notification | Windows notifications (`ffi/windows/attention.rs`) | freedesktop.org service on D-Bus (`freedesktop.rs`) | User Notifications (`notify_macos.rs`) |
//! | Clicking it | brings Veil's window forward | the desktop's own behaviour | opens Veil |
//!
//! The taskbar flash or Dock bounce is not here: the app asks for it through its window
//! (egui's `RequestUserAttention`).
//!
//! Invariants:
//! - Calls may block (D-Bus, WinRT), so the app makes them off its interface thread.
//! - `window` is the main window's title; only this process's window of that name is
//!   touched.

#[cfg(windows)]
use crate::ffi::windows as imp;
#[cfg(target_os = "linux")]
use crate::freedesktop as imp;
#[cfg(target_os = "macos")]
use crate::notify_macos as imp;

/// Shows a notification with a bold `heading` and a `body` line.
pub fn notify(window: &str, heading: &str, body: &str) -> Result<(), String> {
    imp::show_notification(window, heading, body)
}

/// Takes away the notifications [`notify`] showed.
pub fn clear_notifications() {
    imp::clear_notifications();
}
