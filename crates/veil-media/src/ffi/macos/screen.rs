//! What `xcap` cannot say about screen capture on macOS. Part of the macOS `ffi` module; the
//! same rules apply (see `mod.rs`).
//!
//! Purpose: without the Screen Recording permission, macOS gives a picture of the desktop with
//! the windows left out instead of an error, so the permission has to be checked. And `xcap`
//! only sees windows that are on screen, so it cannot tell a minimised window from a closed one.
//!
//! Invariants:
//! - Only a yes or no leaves this file. No window's details are read.
//! - The window list is released on every path.

use std::ffi::c_void;

/// `kCGWindowListOptionIncludingWindow`: list only the given window, wherever it is.
const INCLUDING_WINDOW: u32 = 1 << 3;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFRelease(object: *const c_void);
}

/// True if Veil may record the screen. If not, macOS is asked to offer the permission.
pub fn may_capture_screen() -> bool {
    // SAFETY: neither call takes an argument or hands back anything to manage.
    unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() }
}

/// True while the window `id` exists, on screen or not.
pub fn window_exists(id: u32) -> bool {
    // SAFETY: both arguments are plain values. The result is null or an array we own.
    let list = unsafe { CGWindowListCopyWindowInfo(INCLUDING_WINDOW, id) };
    if list.is_null() {
        return false;
    }
    // SAFETY: `list` is the live array checked above.
    let count = unsafe { CFArrayGetCount(list) };
    // SAFETY: we own `list` under the Copy rule, and it is not used after this.
    unsafe { CFRelease(list) };
    count > 0
}
