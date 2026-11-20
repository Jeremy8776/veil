//! Telling a host who is not looking that someone is waiting in the lobby.
//!
//! Purpose: the People button's badge only helps while Veil is in front. When a guest starts
//! waiting and Veil's window is not focused, the system shows a notification and Veil's
//! taskbar button flashes (Windows), its Dock icon bounces (macOS) or its window is marked
//! urgent (Linux), until the host looks at Veil or nobody is waiting any more.
//!
//! Invariants:
//! - Only new arrivals cause a notification, and only while the window is not focused.
//!   Someone already seen does not cause another, however many frames pass.
//! - The flash and the notifications are cleared once: when the window gets focus, or when
//!   the lobby empties (everyone let in, declined or gone).
//! - [`LobbyAlert::update`] is pure. [`LobbyAlert::apply`] asks the window for attention
//!   through egui and hands notifications to one worker thread, in order, because they can
//!   block (`veil_media::attention`).

use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};

use eframe::egui::{self, UserAttentionType, ViewportCommand};

/// What to do this frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Alert {
    /// A notification to show, with this text.
    pub notify: Option<String>,
    /// Start flashing the taskbar button.
    pub flash: bool,
    /// Stop flashing and take Veil's notifications away.
    pub clear: bool,
}

#[derive(Default)]
pub struct LobbyAlert {
    /// Who was waiting last frame, by id.
    seen: HashSet<u32>,
    /// The taskbar button is flashing or a notification may be showing.
    raised: bool,
    /// The notification thread, started on first use.
    worker: Option<Sender<Job>>,
}

/// Work for the notification thread.
enum Job {
    Show(String),
    Clear,
}

impl LobbyAlert {
    /// Compares who is waiting now with last frame. `lobby` is empty when you are not
    /// hosting a call.
    pub fn update(&mut self, lobby: &[(u32, &str)], focused: bool) -> Alert {
        let new: Vec<&str> = lobby
            .iter()
            .filter(|(id, _)| !self.seen.contains(id))
            .map(|(_, name)| *name)
            .collect();
        self.seen = lobby.iter().map(|(id, _)| *id).collect();
        let mut alert = Alert::default();
        if focused || lobby.is_empty() {
            alert.clear = std::mem::take(&mut self.raised);
        } else if !new.is_empty() {
            alert.notify = Some(wants_to_join(&new));
            alert.flash = true;
            self.raised = true;
        }
        alert
    }

    /// Runs [`Self::update`] and acts on it. `window` is the main window's title.
    pub fn apply(
        &mut self,
        ctx: &egui::Context,
        lobby: &[(u32, &str)],
        focused: bool,
        window: &'static str,
    ) {
        let alert = self.update(lobby, focused);
        if alert.clear {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(UserAttentionType::Reset));
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
                        if let Err(e) = veil_media::attention::notify(window, &text, body) {
                            crate::log::note(&format!("Lobby notification not shown: {e}"));
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
        let first = alert.update(&[(1, "Ana")], false);
        assert_eq!(first.notify.as_deref(), Some("Ana wants to join"));
        assert!(first.flash && !first.clear);
        assert_eq!(alert.update(&[(1, "Ana")], false), Alert::default());
    }

    #[test]
    fn only_the_newcomers_are_named() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false);
        let next = alert.update(&[(1, "Ana"), (2, "Ben")], false);
        assert_eq!(next.notify.as_deref(), Some("Ben wants to join"));
    }

    #[test]
    fn nothing_while_focused_and_seen_people_stay_seen() {
        let mut alert = LobbyAlert::default();
        assert_eq!(alert.update(&[(1, "Ana")], true), Alert::default());
        assert_eq!(alert.update(&[(1, "Ana")], false), Alert::default());
    }

    #[test]
    fn focus_clears_once() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false);
        assert!(alert.update(&[(1, "Ana")], true).clear);
        assert_eq!(alert.update(&[(1, "Ana")], true), Alert::default());
    }

    #[test]
    fn an_empty_lobby_clears_while_in_the_background() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false);
        assert!(alert.update(&[], false).clear);
        assert_eq!(alert.update(&[], false), Alert::default());
    }

    #[test]
    fn someone_who_knocks_again_is_new_again() {
        let mut alert = LobbyAlert::default();
        alert.update(&[(1, "Ana")], false);
        alert.update(&[], false);
        assert!(alert.update(&[(1, "Ana")], false).notify.is_some());
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
