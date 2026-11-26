//! A reminder in the call when someone has waited in the lobby for a while.
//!
//! Purpose: the People badge and the system notification (`lobby_alert.rs`) are easy to miss
//! while the host is talking. Once someone has waited [`AFTER`], the host sees a dialog with
//! Admit and Decline.
//!
//! Invariants:
//! - [`Reminder::update`] is pure: it only compares who is waiting, and since when, with the
//!   time now.
//! - One dialog at a time, and at most one per waiting person. Closing it without a choice
//!   does not bring it back for that person.
//! - The dialog closes by itself when its person is admitted, declined or leaves. Someone who
//!   leaves and knocks again is new again.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::model::clock;
use crate::screens::call::CallView;
use crate::screens::dialog::{self, Choice};
use crate::widgets::Tone;

/// How long someone waits before the host is reminded.
pub const AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct Reminder {
    /// Who has had a reminder, while they are still waiting.
    reminded: HashSet<u32>,
    /// Who the open dialog is for.
    open: Option<u32>,
}

impl Reminder {
    /// Who the dialog is for this frame, if anyone. `lobby` is each waiting person's id and
    /// when they started waiting; empty when you are not hosting.
    pub fn update(&mut self, lobby: &[(u32, Instant)], now: Instant) -> Option<u32> {
        let waiting = |id: &u32| lobby.iter().any(|(w, _)| w == id);
        self.reminded.retain(waiting);
        self.open = self.open.filter(waiting);
        if self.open.is_none() {
            self.open = lobby
                .iter()
                .filter(|(id, since)| {
                    !self.reminded.contains(id) && now.saturating_duration_since(*since) >= AFTER
                })
                .min_by_key(|(_, since)| *since)
                .map(|(id, _)| *id);
            if let Some(id) = self.open {
                self.reminded.insert(id);
            }
        }
        self.open
    }

    /// The host closed the dialog without choosing.
    pub fn dismiss(&mut self) {
        self.open = None;
    }
}

/// Shows the reminder for the host, and acts on Admit or Decline.
pub fn show(view: &mut CallView, ctx: &egui::Context, now: Instant) {
    let lobby: Vec<(u32, Instant)> = if view.call.is_host {
        view.call.lobby.iter().map(|w| (w.id, w.since)).collect()
    } else {
        Vec::new()
    };
    let Some(id) = view.reminder.update(&lobby, now) else {
        return;
    };
    let Some(waiting) = view.call.lobby.iter().find(|w| w.id == id) else {
        return;
    };
    let name = waiting.name.clone();
    let waited = clock(now.saturating_duration_since(waiting.since));
    let safety = waiting.safety.clone();
    let width = dialog::width(ctx.content_rect().width(), 360.0);
    let mut choice = None;
    let open = dialog::show(ctx, "lobby-reminder", "Still waiting", width, |ui| {
        let line = ui.label(format!("{name} has waited {waited}."));
        if !safety.is_empty() {
            line.on_hover_text(format!(
                "Safety number: {safety}\nCompare it with them before letting them in."
            ));
        }
        let (decline, admit) = dialog::pair(
            ui,
            width,
            Choice::new(None, "Decline", Tone::Quiet).hint("Turns them away."),
            Choice::new(None, "Admit", Tone::Primary)
                .hint("Lets them into the call. A new key epoch starts."),
        );
        if admit.clicked() {
            choice = Some(true);
        } else if decline.clicked() {
            choice = Some(false);
        }
    });
    match choice {
        Some(true) => view.admit(id),
        Some(false) => view.call.decline(id),
        None if !open => view.reminder.dismiss(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn nothing_before_the_wait_is_up() {
        let t = Instant::now();
        let mut reminder = Reminder::default();
        assert_eq!(reminder.update(&[(1, t)], at(t, 29)), None);
        assert_eq!(reminder.update(&[(1, t)], at(t, 30)), Some(1));
    }

    #[test]
    fn the_dialog_stays_for_its_person_until_they_go() {
        let t = Instant::now();
        let mut reminder = Reminder::default();
        reminder.update(&[(1, t)], at(t, 30));
        assert_eq!(reminder.update(&[(1, t)], at(t, 90)), Some(1));
        assert_eq!(
            reminder.update(&[], at(t, 91)),
            None,
            "admitted, declined or left"
        );
    }

    #[test]
    fn a_dismissed_reminder_does_not_come_back() {
        let t = Instant::now();
        let mut reminder = Reminder::default();
        reminder.update(&[(1, t)], at(t, 30));
        reminder.dismiss();
        assert_eq!(reminder.update(&[(1, t)], at(t, 300)), None);
    }

    #[test]
    fn one_at_a_time_longest_waiting_first() {
        let t = Instant::now();
        let mut reminder = Reminder::default();
        let lobby = [(2, at(t, 5)), (1, t)];
        assert_eq!(reminder.update(&lobby, at(t, 40)), Some(1));
        assert_eq!(reminder.update(&lobby, at(t, 41)), Some(1), "not a stream");
        let rest = [(2, at(t, 5))];
        assert_eq!(
            reminder.update(&rest, at(t, 42)),
            Some(2),
            "then the next one"
        );
    }

    #[test]
    fn someone_who_knocks_again_is_new_again() {
        let t = Instant::now();
        let mut reminder = Reminder::default();
        reminder.update(&[(1, t)], at(t, 30));
        reminder.dismiss();
        reminder.update(&[], at(t, 31));
        let again = at(t, 40);
        assert_eq!(reminder.update(&[(1, again)], at(t, 60)), None);
        assert_eq!(reminder.update(&[(1, again)], at(t, 70)), Some(1));
    }
}
