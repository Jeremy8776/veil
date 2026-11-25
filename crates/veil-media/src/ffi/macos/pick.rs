//! The macOS open panel (`NSOpenPanel`), for importing a LUT. Part of the macOS `ffi` module;
//! the same rules apply (see `mod.rs`).
//!
//! Invariants:
//! - AppKit panels belong to the main thread, so the panel is queued onto the main queue and
//!   `done` is called there once with the chosen path, or `None` when cancelled.
//! - Only one file can be chosen, never a folder.

use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
use objc2_foundation::NSString;

/// Shows the open panel titled `title` and calls `done` with the result on the main thread.
pub fn pick_file(title: &str, done: impl FnOnce(Option<PathBuf>) + Send + 'static) {
    let title = title.to_string();
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            done(None);
            return;
        };
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&NSString::from_str(&title)));
        if panel.runModal() != NSModalResponseOK {
            done(None);
            return;
        }
        let path = panel
            .URLs()
            .firstObject()
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()));
        done(path);
    });
}
