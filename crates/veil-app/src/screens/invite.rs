//! The host's invite link on screen: the card on an empty stage, how long the link works, and
//! the choice to keep links working until the call ends.
//!
//! Invariants:
//! - The time left is counted from the link's own expiry, so it matches what a guest's Veil
//!   checks when they paste it.
//! - Choosing "until the call ends" (or back) asks the engine for a fresh link; a link already
//!   sent keeps the lifetime it was made with.

use eframe::egui::{self, Rect, RichText, pos2, vec2};

use crate::icons::Icon;
use crate::model::{Action, Call};
use crate::screens::call::CallView;
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card as card_frame, pill};

/// How long the shown link works, for the card, People and the Invite button's tooltip.
pub fn lifetime_text(call: &Call, now_unix: u64) -> String {
    if call.invite_until_end {
        return String::from("Works once, until the call ends.");
    }
    if call.invite_expires == 0 {
        return String::from("Works once.");
    }
    match call.invite_expires.saturating_sub(now_unix) {
        0..60 => String::from("Works once. Expires in under a minute."),
        left => format!("Works once. Expires in {} min.", left.div_ceil(60)),
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The lifetime line and the "until the call ends" box, under a copy button.
pub fn lifetime(view: &mut CallView, ui: &mut egui::Ui) {
    let text = lifetime_text(&view.call, now_unix());
    ui.label(RichText::new(text).small().color(MUTED));
    let mut until_end = view.call.invite_until_end;
    let keep = ui
        .checkbox(&mut until_end, "Keep links working until the call ends")
        .on_hover_text(
            "Each link still lets one person in, and you still admit them. Changing this \
             makes a new link.",
        );
    if keep.changed() {
        view.call.invite_until_end = until_end;
        view.call.outbox.push(Action::InviteUntilEnd(until_end));
    }
}

/// A card on the stage with the invite link and a copy button, while nobody has joined.
pub fn card(view: &mut CallView, ui: &mut egui::Ui, area: Rect) {
    let width = 460.0f32.min(area.width() - 32.0);
    let rect = Rect::from_min_size(
        pos2(area.center().x - width / 2.0, area.bottom() - 280.0),
        vec2(width, 200.0),
    );
    let mut copy = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        card_frame().show(ui, |ui| {
            ui.set_width(width - 40.0);
            ui.label(RichText::new("Invite someone").font(theme::bold(16.0)));
            ui.add_space(4.0);
            let mut link = view.call.invite.as_str();
            ui.add(egui::TextEdit::singleline(&mut link).desired_width(f32::INFINITY));
            ui.add_space(4.0);
            let label = "Copy invite link";
            copy = pill(ui, Some(Icon::Copy), label, Tone::Primary, width - 40.0).clicked();
            ui.add_space(4.0);
            lifetime(view, ui);
        });
    });
    if copy {
        view.copy_invite(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn call(expires: u64, until_end: bool) -> Call {
        let mut call = Call::live("Call".into(), "Host", true, Instant::now());
        call.invite_expires = expires;
        call.invite_until_end = until_end;
        call
    }

    #[test]
    fn the_time_left_rounds_up_to_whole_minutes() {
        assert_eq!(
            lifetime_text(&call(4_600, false), 1_000),
            "Works once. Expires in 60 min."
        );
        assert_eq!(
            lifetime_text(&call(4_600, false), 4_479),
            "Works once. Expires in 3 min."
        );
        assert_eq!(
            lifetime_text(&call(4_600, false), 4_560),
            "Works once. Expires in under a minute."
        );
        assert_eq!(
            lifetime_text(&call(4_600, false), 9_999),
            "Works once. Expires in under a minute.",
            "a replacement is on its way"
        );
    }

    #[test]
    fn until_the_call_ends_shows_no_countdown() {
        assert_eq!(
            lifetime_text(&call(9_999_999, true), 1_000),
            "Works once, until the call ends."
        );
        assert_eq!(lifetime_text(&call(0, false), 1_000), "Works once.");
    }
}
