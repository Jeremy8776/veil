//! Veil's keyboard shortcuts, and how each is written in a tooltip.
//!
//! Purpose: one list, so a shortcut and the text that names it never disagree.
//!
//! Invariants:
//! - Every shortcut is Command+Shift plus a letter: Ctrl+Shift on Windows and Linux, ⇧⌘ on
//!   macOS, where Ctrl+click and Ctrl+letter mean other things.
//! - Tooltips name shortcuts with [`label`], which egui writes in the running system's style.

use eframe::egui::{Context, Key, KeyboardShortcut, Modifiers};

const MODIFIERS: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

/// Mute or unmute in a call.
pub const MUTE: KeyboardShortcut = KeyboardShortcut::new(MODIFIERS, Key::M);
/// Start or stop video in a call.
pub const VIDEO: KeyboardShortcut = KeyboardShortcut::new(MODIFIERS, Key::O);
/// Show or hide call stats.
pub const STATS: KeyboardShortcut = KeyboardShortcut::new(MODIFIERS, Key::D);
/// Open the connection report, anywhere.
pub const REPORT: KeyboardShortcut = KeyboardShortcut::new(MODIFIERS, Key::E);

/// `text` followed by `shortcut` in brackets, as this system writes it.
pub fn label(ctx: &Context, text: &str, shortcut: &KeyboardShortcut) -> String {
    format!("{text} ({})", ctx.format_shortcut(shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_distinct_and_use_command_shift() {
        let all = [MUTE, VIDEO, STATS, REPORT];
        for (i, a) in all.iter().enumerate() {
            assert_eq!(a.modifiers, Modifiers::COMMAND | Modifiers::SHIFT);
            assert!(all[i + 1..].iter().all(|b| b.logical_key != a.logical_key));
        }
    }

    #[test]
    fn labels_name_the_key() {
        let ctx = Context::default();
        let text = label(&ctx, "Microphone", &MUTE);
        assert!(
            text.starts_with("Microphone (") && text.ends_with("M)"),
            "{text}"
        );
    }
}
