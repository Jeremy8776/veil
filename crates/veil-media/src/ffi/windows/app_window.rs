//! Window rules for screen sharing: which windows to offer, and which to keep out of captures.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: `xcap` lists every visible titled window, including tool windows such as graphics
//! overlays and the hidden web views inside apps. Only windows a person would switch to with
//! Alt+Tab are worth offering. Veil's floating sharing bar is hidden from captures, so people
//! watching a whole-screen share do not see it. So is Veil's main window while the share picker
//! is open on it.
//!
//! Invariants:
//! - Other apps' windows are only read: their style and owner. Nothing is sent to them.
//! - Only a window of this process is ever changed.
//! - A window that has gone away is harmless: capturing it fails later.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowThreadProcessId,
    SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_EX_STYLE, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{HSTRING, PCWSTR};

/// This process's top-level window called `title`. Other apps may have a window of the same
/// name, such as a folder called Veil, so each match is checked.
fn own_window(title: &str) -> Option<HWND> {
    let title = HSTRING::from(title);
    let mut after = None;
    loop {
        // SAFETY: `title` is a valid terminated string that outlives the call, and `after` is
        // absent or a handle Windows just returned. Windows reports failure at the end.
        let window =
            unsafe { FindWindowExW(None, after, PCWSTR::null(), PCWSTR(title.as_ptr())) }.ok()?;
        let mut process = 0;
        // SAFETY: the handle is passed by value and `process` is a valid out pointer.
        unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
        if process == std::process::id() {
            return Some(window);
        }
        after = Some(window);
    }
}

/// Keeps this process's window called `title` out of screen captures, or lets it back in.
/// False if there is no such window yet, or this version of Windows refuses.
pub fn set_hidden_from_capture(title: &str, hidden: bool) -> bool {
    let Some(window) = own_window(title) else {
        return false;
    };
    let affinity = if hidden { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    // SAFETY: the window belongs to this process, as the call requires.
    unsafe { SetWindowDisplayAffinity(window, affinity) }.is_ok()
}

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
