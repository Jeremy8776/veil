//! The Security side panel: is this call encrypted, and who have you checked?
//!
//! Purpose: one short row per person, with the next step on it. Safety numbers stay hidden
//! until you choose to verify someone, so the panel reads as a checklist, not a wall of digits.
//!
//! Invariants:
//! - One person is open at a time (`CallView::verifying`).
//! - Nobody can be marked verified without a safety number to compare.
//! - "Verified" is only a label for a device you compared. The host still admits.
//! - In a demo call the panel says the numbers are made up.
//! - How someone is connected is a tooltip: it says nothing about who they are.

use eframe::egui::{
    self, Align2, CornerRadius, FontId, Margin, Response, RichText, Role, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use crate::icons::Icon;
use crate::model::{LinkPath, Participant};
use crate::screens::call::CallView;
use crate::theme::{self, ACCENT, MUTED, RAISED, RAISED_HOVER, TEXT};
use crate::widgets::{self, Tone, pill};

const ROW_H: f32 = 48.0;
/// Lines the details up under the name, past the avatar.
const DETAILS_LEFT: i8 = 46;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Verified,
    Unverified,
    /// No safety number yet, so there is nothing to compare.
    Connecting,
}

impl Status {
    fn of(verified: bool, safety: &str) -> Self {
        if verified {
            Self::Verified
        } else if safety.is_empty() {
            Self::Connecting
        } else {
            Self::Unverified
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Verified => "Verified",
            Self::Unverified => "Verify",
            Self::Connecting => "Connecting",
        }
    }
}

/// The line under "Encrypted".
fn summary(verified: usize, total: usize) -> String {
    match (verified, total) {
        (_, 0) => String::from("Nobody else is here yet"),
        (v, t) if v == t => String::from("Everyone verified"),
        (_, 1) => String::from("Not verified yet"),
        (v, t) => format!("{v} of {t} people verified"),
    }
}

/// Which person is open after `id`'s row is clicked.
fn toggled(open: Option<u32>, id: u32) -> Option<u32> {
    if open == Some(id) { None } else { Some(id) }
}

fn connection_hint(path: LinkPath) -> &'static str {
    match path {
        LinkPath::Direct => "Connected directly.",
        LinkPath::Punched => "Connected directly, through both routers.",
        LinkPath::Relayed => "Connected through a relay. It cannot read the call.",
        LinkPath::Forwarder => "Connected through a forwarder. It cannot read the call.",
        LinkPath::Pending => "Still connecting.",
    }
}

pub fn show(view: &mut CallView, ui: &mut Ui) {
    let others = || view.call.participants.iter().filter(|p| !p.is_you);
    let total = others().count();
    let verified = others().filter(|p| p.verified).count();
    header(ui, view, verified, total);
    ui.add_space(8.0);

    let mut open = view.verifying;
    let mut change = None;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for person in others() {
                let is_open = open == Some(person.id);
                if row(ui, person, is_open).clicked() {
                    open = toggled(open, person.id);
                }
                if is_open && let Some(verified) = details(ui, person) {
                    change = Some((person.id, verified));
                }
            }
        });
    if let Some((id, verified)) = change {
        view.call.set_verified(id, verified);
        open = None;
    }
    view.verifying = open;
}

fn header(ui: &mut Ui, view: &CallView, verified: usize, total: usize) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(26.0, 36.0), Sense::hover());
        Icon::Shield.paint(ui.painter(), rect.center(), 20.0, theme::SPEAKING);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let tip = format!(
                "Encrypted on each device. Relays and forwarders cannot open it.\n\
                 Key {}: a new key starts whenever someone joins or leaves.",
                view.call.epoch
            );
            ui.label(RichText::new("Encrypted").color(theme::SPEAKING))
                .on_hover_text(tip);
            ui.horizontal(|ui| {
                ui.label(RichText::new(summary(verified, total)).small().color(MUTED));
                if view.call.demo {
                    ui.label(RichText::new("demo").small().color(theme::WARN))
                        .on_hover_text("These safety numbers are made up for the demo call.");
                }
                let (rect, info) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                Icon::Info.paint(ui.painter(), rect.center(), 12.0, MUTED);
                info.on_hover_text(
                    "Verifying proves who you are talking to. \
                     If your safety numbers match, nobody is in between.",
                );
            });
        });
    });
}

/// One person: avatar, name, and what to do next. Click to open or close their numbers.
fn row(ui: &mut Ui, person: &Participant, open: bool) -> Response {
    let status = Status::of(person.verified, &person.safety);
    let can_open = status != Status::Connecting;
    let sense = if can_open { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), sense);
    let label = format!("{}: {}", person.name, status.label());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, can_open, &label));
    let hint = connection_hint(person.path);
    if !ui.is_rect_visible(rect) {
        return response.on_hover_text(hint);
    }

    let painter = ui.painter();
    let corners = CornerRadius::same(6);
    if can_open && response.hovered() {
        painter.rect_filled(rect, corners, RAISED_HOVER);
    } else if open {
        painter.rect_filled(rect, corners, RAISED);
    }
    if response.has_focus() {
        painter.rect_stroke(rect, corners, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
    }
    let y = rect.center().y;
    widgets::avatar(painter, pos2(rect.left() + 22.0, y), 16.0, &person.name);

    let mut right = rect.right() - 8.0;
    if can_open {
        widgets::chevron(painter, pos2(right - 5.0, y), open, MUTED);
        right -= 18.0;
    }
    let colour = match status {
        Status::Verified => theme::SPEAKING,
        Status::Unverified => ACCENT,
        Status::Connecting => MUTED,
    };
    let text = painter.text(
        pos2(right, y),
        Align2::RIGHT_CENTER,
        status.label(),
        FontId::proportional(12.5),
        colour,
    );
    let mut name_right = text.left() - 8.0;
    if status == Status::Verified {
        let center = pos2(text.left() - 9.0, y);
        Icon::Shield.paint(painter, center, 12.0, colour);
        name_right -= 16.0;
    }

    let name_left = rect.left() + f32::from(DETAILS_LEFT);
    let mut job = egui::text::LayoutJob::single_section(
        person.name.clone(),
        egui::TextFormat::simple(FontId::proportional(14.0), TEXT),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(name_right - name_left);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let elided = galley.elided;
    painter.galley(pos2(name_left, y - galley.size().y / 2.0), galley, TEXT);
    if elided {
        response.on_hover_text(format!("{}\n{hint}", person.name))
    } else {
        response.on_hover_text(hint)
    }
}

/// The open person's safety number. Returns their new verified state if it was changed.
fn details(ui: &mut Ui, person: &Participant) -> Option<bool> {
    let margin = Margin {
        left: DETAILS_LEFT,
        right: 6,
        top: 4,
        bottom: 12,
    };
    egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if !person.verified {
                ui.label(
                    RichText::new("Compare by phone or in person.")
                        .small()
                        .color(MUTED),
                )
                .on_hover_text("Use something other than this call, and read the numbers aloud.");
                ui.add_space(2.0);
            }
            let groups: Vec<&str> = person.safety.split(' ').collect();
            for line in groups.chunks(4) {
                ui.label(RichText::new(line.join("  ")).monospace().size(13.0));
            }
            ui.add_space(6.0);
            let width = ui.available_width();
            if person.verified {
                let forget = pill(ui, None, "Forget", Tone::Quiet, width)
                    .on_hover_text("Stop treating this device as verified.");
                return forget.clicked().then_some(false);
            }
            let matched = pill(ui, None, "They match", Tone::Primary, width)
                .on_hover_text("Remembers this device for future calls.");
            ui.label(
                RichText::new("Different? Leave the call.")
                    .small()
                    .color(theme::WARN),
            )
            .on_hover_text("Someone may be between you and this person.");
            matched.clicked().then_some(true)
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nobody_can_be_verified_without_a_number() {
        assert_eq!(Status::of(false, ""), Status::Connecting);
        assert_eq!(Status::of(false, "12345"), Status::Unverified);
    }

    #[test]
    fn a_verified_person_stays_verified_while_their_number_loads() {
        assert_eq!(Status::of(true, ""), Status::Verified);
        assert_eq!(Status::of(true, "12345"), Status::Verified);
    }

    #[test]
    fn the_summary_counts_people() {
        assert_eq!(summary(0, 0), "Nobody else is here yet");
        assert_eq!(summary(0, 1), "Not verified yet");
        assert_eq!(summary(1, 4), "1 of 4 people verified");
        assert_eq!(summary(4, 4), "Everyone verified");
    }

    #[test]
    fn one_person_is_open_at_a_time() {
        assert_eq!(toggled(None, 1), Some(1));
        assert_eq!(toggled(Some(1), 2), Some(2));
        assert_eq!(toggled(Some(2), 2), None);
    }

    #[test]
    fn relayed_calls_say_the_relay_cannot_read_them() {
        for path in [LinkPath::Relayed, LinkPath::Forwarder] {
            assert!(connection_hint(path).contains("cannot read"));
        }
    }
}
