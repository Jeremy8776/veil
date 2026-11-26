//! Desktop notifications on Linux, through the freedesktop.org notification service on the
//! session D-Bus (GNOME, KDE, Xfce, and the notification daemons of tiling desktops).
//!
//! Purpose: the Linux side of [`crate::attention`].
//!
//! Invariants:
//! - Each call is blocking, so callers run it off the interface thread: a missing
//!   notification daemon can make D-Bus wait for its timeout.
//! - The summary is plain text by the specification. The body may be read as markup, so it
//!   is escaped first.
//! - Only notifications this process showed are closed, by the ids the service returned.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use zbus::blocking::Connection;
use zbus::zvariant::Value;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Veil's desktop entry and icon name (`packaging/linux`), so the notification carries
/// Veil's name and icon and follows its notification settings.
const DESKTOP_ENTRY: &str = "io.github.jeremy8776.veil";

/// Notifications shown and not yet closed, by the service's id.
static SHOWN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Shows a notification. `_window` is unused: the desktop raises Veil from the urgency hint.
pub fn show_notification(_window: &str, heading: &str, body: &str) -> Result<(), String> {
    let id = notify(heading, &escape_markup(body)).map_err(|e| e.to_string())?;
    SHOWN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(id);
    Ok(())
}

/// Closes the notifications [`show_notification`] showed. Failure is harmless: the
/// notification stays until the person dismisses it.
pub fn clear_notifications() {
    let ids = std::mem::take(&mut *SHOWN.lock().unwrap_or_else(PoisonError::into_inner));
    if ids.is_empty() {
        return;
    }
    let Ok(bus) = Connection::session() else {
        return;
    };
    for id in ids {
        let _ = bus.call_method(Some(SERVICE), PATH, Some(SERVICE), "CloseNotification", &id);
    }
}

fn notify(summary: &str, body: &str) -> zbus::Result<u32> {
    let bus = Connection::session()?;
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from(DESKTOP_ENTRY));
    // Normal urgency: shown like a chat message, not held on screen like an alarm.
    hints.insert("urgency", Value::U8(1));
    // Many daemons play a sound only when asked; this is the theme's chat message sound.
    hints.insert("sound-name", Value::from("message-new-instant"));
    hints.insert("category", Value::from("im.received"));
    let actions: Vec<&str> = Vec::new();
    // Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout),
    // with -1 leaving the timeout to the desktop.
    let reply = bus.call_method(
        Some(SERVICE),
        PATH,
        Some(SERVICE),
        "Notify",
        &(
            "Veil",
            0u32,
            DESKTOP_ENTRY,
            summary,
            body,
            actions,
            hints,
            -1i32,
        ),
    )?;
    reply.body().deserialize::<u32>()
}

/// Escapes the characters the notification body's markup gives meaning to.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_shown_as_text() {
        assert_eq!(
            escape_markup("<a href=\"x\">A & B</a>"),
            "&lt;a href=\"x\"&gt;A &amp; B&lt;/a&gt;"
        );
        assert_eq!(
            escape_markup("Open Veil to let them in."),
            "Open Veil to let them in."
        );
    }
}
