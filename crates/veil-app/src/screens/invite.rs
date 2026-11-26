//! The host's invite link on screen: the card on an empty stage, how long the link works, and
//! the choice to keep links working until the call ends.
//!
//! Invariants:
//! - The time left is counted from the link's own expiry, so it matches what a guest's Veil
//!   checks when they paste it.
//! - Choosing "until the call ends" (or back) asks the engine for a fresh link; a link already
//!   sent keeps the lifetime it was made with.
//! - "Full link" shows only when the link on screen is a short one, and "QR code" only in a
//!   real call.
//! - The card is measured each frame and sits 80 points above the stage's bottom edge,
//!   never above its top.

use eframe::egui::{self, Rect, RichText, pos2, vec2};

use crate::icons::Icon;
use crate::model::{Action, Call};
use crate::screens::call::CallView;
use crate::screens::dialog::{Choice, one, pair};
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card as card_frame, pill, toggle_row};

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

/// Under a copy button, one centred column: how long the link works, the nearby code, the QR
/// and full link as a pair of equal buttons, and the "until the call ends" switch drawn by
/// `switch` (centred on the stage card, a list row in the People panel).
pub fn lifetime(
    view: &mut CallView,
    ui: &mut egui::Ui,
    switch: impl FnOnce(&mut egui::Ui, &mut bool, &str) -> egui::Response,
) {
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = theme::GAP;
        let width = ui.available_width();
        let text = lifetime_text(&view.call, now_unix());
        ui.label(RichText::new(text).small().color(MUTED));
        if !view.call.nearby_code.is_empty() {
            ui.label(
                RichText::new(format!("Nearby code {}", view.call.nearby_code))
                    .font(theme::bold(14.0)),
            )
            .on_hover_text(
                "Guests who find this call nearby type this code to get the invite. Tell it \
                 only to people you want to invite.",
            );
        }
        let qr = Choice::new(None, "QR code", Tone::Quiet).hint(QR_HINT);
        let full = Choice::new(Some(Icon::Link), "Full link", Tone::Quiet).hint(FULL_HINT);
        let (show_qr, copy_full) =
            match extras(view.call.demo, view.call.invite != view.call.invite_full) {
                Extras::Both => {
                    let (a, b) = pair(ui, width, qr, full);
                    (a.clicked(), b.clicked())
                }
                Extras::Qr => (one(ui, qr, width).clicked(), false),
                Extras::FullLink => (false, one(ui, full, width).clicked()),
                Extras::None => (false, false),
            };
        if show_qr {
            crate::invite_qr::open(ui.ctx());
        }
        if copy_full {
            view.copy_full_invite(ui.ctx());
        }
        let mut until_end = view.call.invite_until_end;
        let keep = switch(ui, &mut until_end, "Keep until the call ends").on_hover_text(
            "Links work until the call ends instead of expiring. Each link still lets one \
             person in, and you still admit them. Changing this makes a new link.",
        );
        if keep.changed() {
            view.call.invite_until_end = until_end;
            view.call.outbox.push(Action::InviteUntilEnd(until_end));
        }
    });
}

const QR_HINT: &str = "Show the whole invitation as a QR code. No server is needed to read it.";
const FULL_HINT: &str = "Copy a longer link for a guest who cannot connect with the short one.";

/// Which of the QR and full link buttons show.
#[derive(Debug, PartialEq, Eq)]
enum Extras {
    Both,
    Qr,
    FullLink,
    None,
}

/// A demo call has no real invitation for a QR; the full link only differs when the shown
/// one is short.
fn extras(demo: bool, short: bool) -> Extras {
    match (!demo, short) {
        (true, true) => Extras::Both,
        (true, false) => Extras::Qr,
        (false, true) => Extras::FullLink,
        (false, false) => Extras::None,
    }
}

/// How far the card sits above the stage's bottom edge.
const CARD_BOTTOM: f32 = 80.0;
/// The card's height before it has been measured once.
const CARD_GUESS: f32 = 300.0;

/// The card's top edge for a stage ending at `bottom`: lifted clear of the bottom edge, but
/// never above the stage's top.
fn card_top(top: f32, bottom: f32, height: f32) -> f32 {
    (bottom - CARD_BOTTOM - height).max(top + theme::GAP)
}

/// A card on the stage with the invite link and a copy button, while nobody has joined.
pub fn card(view: &mut CallView, ui: &mut egui::Ui, area: Rect) {
    let width = 460.0f32.min(area.width() - 32.0);
    let id = ui.scope_id().with("invite-card-height");
    let height = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(CARD_GUESS);
    let rect = Rect::from_min_size(
        pos2(
            area.center().x - width / 2.0,
            card_top(area.top(), area.bottom(), height),
        ),
        vec2(width, height),
    );
    let mut copy = false;
    let shown = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        card_frame().show(ui, |ui| {
            ui.set_width(width - 40.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = theme::GAP;
                ui.label(RichText::new("Invite someone").font(theme::bold(16.0)));
                let mut link = view.call.invite.as_str();
                ui.add(
                    egui::TextEdit::singleline(&mut link)
                        .align(egui::Align2::CENTER_CENTER)
                        .desired_width(f32::INFINITY),
                );
                let label = "Copy invite link";
                copy = pill(ui, Some(Icon::Copy), label, Tone::Primary, width - 40.0).clicked();
                lifetime(view, ui, |ui, on, label| toggle_row(ui, on, label, ""));
            });
        })
    });
    let measured = shown.response.rect.height();
    if (measured - height).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, measured));
        ui.ctx().request_repaint();
    }
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

    #[test]
    fn the_qr_needs_a_real_call_and_the_full_link_a_short_one() {
        assert_eq!(extras(false, true), Extras::Both);
        assert_eq!(extras(false, false), Extras::Qr);
        assert_eq!(extras(true, true), Extras::FullLink);
        assert_eq!(extras(true, false), Extras::None);
    }

    #[test]
    fn the_card_sits_above_the_bottom_edge_but_never_above_the_stage() {
        assert_eq!(card_top(0.0, 1000.0, 300.0), 620.0);
        assert_eq!(card_top(100.0, 400.0, 300.0), 100.0 + theme::GAP);
    }
}
