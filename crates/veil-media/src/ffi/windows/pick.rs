//! The Windows file open dialog (`IFileOpenDialog`), for importing a LUT.
//!
//! Invariants:
//! - Runs on its own thread with COM initialised as a single-threaded apartment, as the
//!   dialog needs, and balanced before returning. It blocks only that thread.
//! - Every COM pointer is a `windows` interface type, released exactly once. The path string
//!   the dialog allocates is copied and then freed with `CoTaskMemFree`.

use std::path::PathBuf;

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, w};

use crate::MediaError;

/// Shows the dialog for one file of `pattern` (such as `*.cube`), labelled `kind`.
/// `Ok(None)` when the person cancels.
pub fn pick_file(title: &str, kind: &str, pattern: &str) -> Result<Option<PathBuf>, MediaError> {
    // SAFETY: initialises COM for this thread only; balanced below when it succeeded.
    let com =
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
    let result = show(title, kind, pattern);
    if com {
        // SAFETY: balances the successful CoInitializeEx above, on the same thread.
        unsafe { CoUninitialize() };
    }
    result
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn show(title: &str, kind: &str, pattern: &str) -> Result<Option<PathBuf>, MediaError> {
    let failed = |_| MediaError::Unavailable("file dialog");
    // SAFETY: creates the system file dialog; the interface owns the reference.
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }.map_err(failed)?;
    let (title, kind, pattern) = (wide(title), wide(kind), wide(pattern));
    let filters = [COMDLG_FILTERSPEC {
        pszName: windows::core::PCWSTR(kind.as_ptr()),
        pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
    }];
    // SAFETY: the strings are NUL-terminated and outlive these calls; the dialog copies them.
    unsafe {
        dialog.SetFileTypes(&filters).map_err(failed)?;
        dialog
            .SetTitle(windows::core::PCWSTR(title.as_ptr()))
            .map_err(failed)?;
        dialog.SetOkButtonLabel(w!("Import")).map_err(failed)?;
        let options = dialog.GetOptions().map_err(failed)?;
        dialog
            .SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)
            .map_err(failed)?;
    }
    // SAFETY: a modal dialog with no owner window; it runs its own message loop on this thread.
    match unsafe { dialog.Show(None) } {
        Ok(()) => {}
        Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
        Err(e) => return Err(failed(e)),
    }
    // SAFETY: Show succeeded, so the dialog holds one chosen item.
    let item = unsafe { dialog.GetResult() }.map_err(failed)?;
    // SAFETY: FOS_FORCEFILESYSTEM means the item has a file system path.
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(failed)?;
    // SAFETY: `name` is a NUL-terminated string from the dialog, copied before it is freed
    // once, here.
    let path = unsafe {
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    };
    path.map(|p| Some(PathBuf::from(p)))
        .map_err(|_| MediaError::Malformed("file path"))
}
