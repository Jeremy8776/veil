//! Telling a host who is not looking that someone is waiting in the lobby.
//!
//! Purpose: the People button's badge only helps while the host is looking at Veil. When a
//! guest starts waiting, the system shows a notification with its sound, and while Veil's
//! window is not focused its taskbar button flashes (Windows), its Dock icon bounces (macOS)
//! or its window is marked urgent (Linux).
//!
//! Invariants:
//! - Every new arrival causes one notification, focused or not: a focused window can still
//!   be on another screen or covered. Someone already seen does not cause another, however
//!   many passes run.
//! - The flash stops once the window gets focus. Notifications stay until the lobby empties
//!   (everyone let in, declined or gone), so focusing Veil does not take a banner away before
//!   the host has read it.
//! - With Lobby alerts off nothing is shown, and everyone waiting counts as seen, so turning
//!   it back on does not announce them.
//! - The app runs this from `App::logic`, which eframe also runs while the window is
//!   minimised or hidden, when it skips `App::ui`.
//! - [`LobbyAlert::update`] is pure. [`LobbyAlert::apply`] asks the window for attention
//!   through egui and hands notifications to one worker thread, in order, because they can
//!   block (`veil_media::attention`).

use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};

use eframe::egui::{self, UserAttentionType, ViewportCommand};

/// What to do this pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Alert {
    /// A notification to show, with this text.
    pub notify: Option<String>,
    /// Start flashing the taskbar button.
    pub flash: bool,
    /// Stop flashing the taskbar button.
    pub stop_flash: bool,
    /// Take Veil's notifications away.
    pub clear: bool,
}

#[derive(Default)]
pub struct LobbyAlert {
    /// Who was waiting last pass, by id.
    seen: HashSet<u32>,
    /// The taskbar button is flashing.
    flashing: bool,
    /// A notification may be showing.
    notified: bool,
    /// The notification thread, started on first use.
    worker: Option<Sender<Job>>,
}

/// Work for the notification thread.
enum Job {
    Show(String),
    Clear,
}

impl LobbyAlert {
    /// Compares who is waiting now with the last pass. `lobby` is empty when you are not
    /// hosting a call; `enabled` is the Lobby alerts switch.
    pub fn update(&mut self, lobby: &[(u32, &str)], focused: bool, enabled: bool) -> Alert {
        let new: Vec<&str> = lobby
            .iter()
            .filter(|(id, _)| !self.seen.contains(id))
            .map(|(_, name)| *name)
            .collect();
        self.seen = lobby.iter().map(|(id, _)| *id).collect();
        let mut alert = Alert::default();
        if enabled && !new.is_empty() {
            alert.notify = Some(wants_to_join(&new));
            self.notified = true;
            if !focused {
                alert.flash = true;
                self.flashing = true;
            }
        }
        if focused || lobby.is_empty() || !enabled {
            alert.stop_flash = std::mem::take(&mut self.flashing);
        }
        if lobby.is_empty() || !enabled {
            alert.clear = std::mem::take(&mut self.notified);
        }
        alert
    }

    /// Runs [`Self::update`] and acts on it. `window` is the main window's title.
    pub fn apply(
        &mut self,
        ctx: &egui::Context,
        lobby: &[(u32, &str)],
        focused: bool,
        enabled: bool,
        window: &'static str,
    ) {
        let alert = self.update(lobby, focused, enabled);
        if alert.stop_flash {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                UserAttentionType::Reset,
            ));
        }
        if alert.clear {
            self.send(Job::Clear, window);
        }
        if let Some(text) = alert.notify {
            self.send(Job::Show(text), window);
        }
        if alert.flash {
            let attention = UserAttentionType::Critical;
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(attention));
        }
    }

    fn send(&mut self, job: Job, window: &'static str) {
        let worker = self.worker.get_or_insert_with(|| start_worker(window));
        // A worker that could not start drops its end; the flash still works.
        let _ = worker.send(job);
    }
}

/// The thread that shows and clears notifications. It ends when [`LobbyAlert`] is dropped.
fn start_worker(window: &'static str) -> Sender<Job> {
    let (sender, jobs) = mpsc::channel::<Job>();
    let started = std::thread::Builder::new()
        .name("lobby-alert".into())
        .spawn(move || {
            for job in jobs {
                match job {
                    Job::Show(text) => {
                        let body = "Open Veil to let them in.";
                        // The log says whether the system took it, never who is waiting.
                        match veil_media::attention::notify(window, &text, body) {
                            Ok(()) => crate::log::note("Lobby notification handed to the system"),
                            Err(e) => {
                                crate::log::note(&format!("Lobby notification not shown: {e}"))
                            }
                        }
                    }
                    Job::Clear => veil_media::attention::clear_notifications(),
                }
            }
        });
    if let Err(e) = started {
        crate::log::note(&format!("Lobby notifications off: {e}"));
    }
    sender
}

/// "Ana wants to join", or "Ana and 2 others want to join".
pub fn wants_to_join(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("{one} wants to join"),
        [first, rest @ ..] => match rest.len() {
            1 => format!("{first} and 1 other want to join"),
            n => format!("{first} and {n} others want to join"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_arrival_notifies_once_while_in_the_background() {
        let mut alert = LobbyAlert::default();
        let first = alert.update(&[(1, "Ana")], false, true);
        assert_eq!(first.notify.as_deref(), Some("Ana wants to join"));
        assert!(first.flash && !first.clear && !first.stop_flash);
        assert_eq!(alert.update(&[(1, "Ana")], false, true), Alert::default());
    }

    #[test]
    fn a_focused_window_still_notifies_but_does_not_flash() {
        let mut alert = LobbyAlert::default();
        let first = alert.update(&[(1, "Ana")], true, true);
        assert_eq!(first.notify.as_deref(), Some("Ana wants to join"));
        assert!(!first.flash);
        assert_eq!(alert.update(&[(1, "Ana")], true, true), Alert::default());
    }

    #[test]
    fn only_the_newcomers_are_named() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false, true);
        let next = alert.update(&[(1, "Ana"), (2, "Ben")], false, true);
        assert_eq!(next.notify.as_deref(), Some("Ben wants to join"));
    }

    #[test]
    fn focus_stops_the_flash_once_and_keeps_the_notification() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false, true);
        let looked = alert.update(&[(1, "Ana")], true, true);
        assert!(looked.stop_flash && !looked.clear);
        assert_eq!(alert.update(&[(1, "Ana")], true, true), Alert::default());
    }

    #[test]
    fn an_empty_lobby_clears_once() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false, true);
        let empty = alert.update(&[], false, true);
        assert!(empty.clear && empty.stop_flash);
        assert_eq!(alert.update(&[], false, true), Alert::default());
    }

    #[test]
    fn someone_who_knocks_again_is_new_again() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false, true);
        alert.update(&[], false, true);
        assert!(alert.update(&[(1, "Ana")], false, true).notify.is_some());
    }

    #[test]
    fn switched_off_shows_nothing_and_counts_everyone_as_seen() {
        let mut alert = LobbyAlert::default();
        assert_eq!(alert.update(&[(1, "Ana")], false, false), Alert::default());
        assert_eq!(alert.update(&[(1, "Ana")], false, true), Alert::default());
    }

    #[test]
    fn switching_off_takes_away_what_was_showing() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false, true);
        let off = alert.update(&[(1, "Ana")], false, false);
        assert!(off.clear && off.stop_flash);
    }

    #[test]
    fn several_at_once_are_counted() {
        assert_eq!(
            wants_to_join(&["Ana", "Ben"]),
            "Ana and 1 other want to join"
        );
        assert_eq!(
            wants_to_join(&["Ana", "Ben", "Cy"]),
            "Ana and 2 others want to join"
        );
    }
}
