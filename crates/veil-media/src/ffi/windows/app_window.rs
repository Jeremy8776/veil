//! Tells application windows from overlays and helpers, for the screen share picker.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: `xcap` lists every visible titled window, including tool windows such as graphics
//! overlays and the hidden web views inside apps. Only windows a person would switch to with
//! Alt+Tab are worth offering.
//!
//! Invariants:
//! - Only reads a window's style and owner. Nothing is sent to the window.
//! - A window that has gone away is harmless: capturing it fails later.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, WINDOW_EX_STYLE, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// True if the window with this id (`xcap`'s id, the handle's low 32 bits, which are all of its
/// meaningful bits) is an application window.
pub fn is_app_window(id: u32) -> bool {
    let window = HWND(id as usize as *mut c_void);
    // SAFETY: both calls take a handle by value and only read window data. Windows checks the
    // handle and reports failure for one that is not, or is no longer, a window.
    let (style, owner) =
        unsafe { (GetWindowLongW(window, GWL_EXSTYLE), GetWindow(window, GW_OWNER)) };
    is_app_style(WINDOW_EX_STYLE(style as u32), owner.is_ok())
}

/// The rule Alt+Tab follows: tool, non-activating and owned windows are left out, unless the
/// window explicitly asks to be treated as an application.
fn is_app_style(style: WINDOW_EX_STYLE, owned: bool) -> bool {
    let helper = owned || style.contains(WS_EX_TOOLWINDOW) || style.contains(WS_EX_NOACTIVATE);
    style.contains(WS_EX_APPWINDOW) || !helper
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_windows_count_and_helpers_do_not() {
        assert!(is_app_style(WINDOW_EX_STYLE(0), false));
        assert!(!is_app_style(WS_EX_TOOLWINDOW, false));
        assert!(!is_app_style(WS_EX_NOACTIVATE, false));
        assert!(!is_app_style(WINDOW_EX_STYLE(0), true));
    }

    #[test]
    fn a_window_can_ask_to_count_as_an_application() {
        assert!(is_app_style(WS_EX_APPWINDOW | WS_EX_TOOLWINDOW, true));
    }
}
