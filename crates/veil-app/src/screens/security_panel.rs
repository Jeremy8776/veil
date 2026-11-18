//! The Security panel: is this call encrypted, and who have you checked?
//!
//! Purpose: checking someone should not mean reading 60 digits. Each person copies their code,
//! sends it through another app and pastes the one they get back. Veil compares all 60 digits
//! (`veil_security::safety`) and marks the person verified when they match.
//!
//! Invariants:
//! - One person is open at a time (`CallView::verifying`).
//! - Nobody can be marked verified without a safety number to compare.
//! - Your own code pasted back never counts as a match.
//! - Comparing digits by hand stays available, for someone on an older version of Veil.
//! - "Verified" is only a label for a device you compared. The host still admits.
//! - In a demo call the panel says the codes are made up.
//! - How someone is connected is a tooltip: it says nothing about who they are.

use eframe::egui::{
    self, Align2, Button, CornerRadius, FontId, Margin, Response, RichText, Role, ScrollArea,
    Sense, Stroke, StrokeKind, TextEdit, Ui, WidgetInfo, pos2, vec2,
};
use veil_security::safety::{CodeCheck, check_code, share_code};

use crate::icons::Icon;
use crate::model::{LinkPath, Participant};
use crate::screens::call::CallView;
use crate::theme::{self, ACCENT, MUTED, RAISED, RAISED_HOVER, TEXT};
use crate::widgets::{self, Tone, pill};

const ROW_H: f32 = 48.0;
const NAME_LEFT: f32 = 46.0;
/// Far longer than a share code, so a stray paste cannot grow without limit.
const PASTE_LIMIT: usize = 200;

/// The person open in the panel, and what has been entered for them.
pub struct Verifying {
    id: u32,
    pasted: String,
    by_hand: bool,
}

impl Verifying {
    fn new(id: u32) -> Self {
        Self {
            id,
            pasted: String::new(),
            by_hand: false,
        }
    }
}

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
            Self::Unverified => "Not verified",
            Self::Connecting => "Connecting",
        }
    }
}

/// What the person did in the open section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Copied,
    Confirmed,
    Forgot,
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

/// Who is open after `id`'s row is clicked. Opening someone starts with an empty form.
fn toggled(open: Option<Verifying>, id: u32) -> Option<Verifying> {
    match open {
        Some(current) if current.id == id => None,
        _ => Some(Verifying::new(id)),
    }
}

/// What to say about a pasted code, and in which colour. `None` when nothing needs saying.
fn paste_message(check: CodeCheck) -> Option<(&'static str, &'static str, egui::Color32)> {
    match check {
        CodeCheck::Match => None,
        CodeCheck::OwnCode => Some((
            "That is your own code.",
            "Paste the code they sent you.",
            theme::WARN,
        )),
        CodeCheck::Different => Some((
            "Codes do not match.",
            "Check it came from them. If it did, someone may be listening: leave the call.",
            theme::DANGER_HOVER,
        )),
        CodeCheck::NotACode => Some((
            "That is not a Veil code.",
            "A code starts with VEIL-A or VEIL-B.",
            MUTED,
        )),
    }
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

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

pub fn show(view: &mut CallView, ui: &mut Ui) {
    let demo = view.call.demo;
    let others = || view.call.participants.iter().filter(|p| !p.is_you);
    let total = others().count();
    let verified = others().filter(|p| p.verified).count();
    header(ui, view.call.epoch, demo, verified, total);
    ui.add_space(10.0);

    let mut open = view.verifying.take();
    let mut done = None;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for person in others() {
                let is_open = open.as_ref().is_some_and(|o| o.id == person.id);
                if row(ui, person, is_open).clicked() {
                    open = toggled(open.take(), person.id);
                } else if let Some(state) = open.as_mut().filter(|o| o.id == person.id)
                    && let Some(step) = details(ui, person, demo, state)
                {
                    done = Some((person.id, person.name.clone(), step));
                }
            }
        });
    view.verifying = open;
    match done {
        Some((_, _, Step::Copied)) => view.notify("Your code is copied"),
        Some((id, name, Step::Confirmed)) => {
            view.call.set_verified(id, true);
            view.verifying = None;
            view.notify(&format!("{name} is verified"));
        }
        Some((id, _, Step::Forgot)) => {
            view.call.set_verified(id, false);
            view.verifying = None;
        }
        None => {}
    }
}

fn header(ui: &mut Ui, epoch: u64, demo: bool, verified: usize, total: usize) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(26.0, 36.0), Sense::hover());
        Icon::Shield.paint(ui.painter(), rect.center(), 20.0, theme::SPEAKING);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let tip = format!(
                "Encrypted on each device. Relays and forwarders cannot open it.\n\
                 Key {epoch}: a new key starts whenever someone joins or leaves."
            );
            ui.label(RichText::new("Encrypted").color(theme::SPEAKING))
                .on_hover_text(tip);
            ui.horizontal(|ui| {
                ui.label(RichText::new(summary(verified, total)).small().color(MUTED));
                if demo {
                    ui.label(RichText::new("demo").small().color(theme::WARN))
                        .on_hover_text("These codes are made up for the demo call.");
                }
                let (rect, info) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                Icon::Info.paint(ui.painter(), rect.center(), 12.0, MUTED);
                info.on_hover_text(
                    "Verifying is optional. It proves nobody is secretly between you and \
                     this person.",
                );
            });
        });
    });
}

/// One person: avatar, name and status. Click to open or close their section.
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
    let colour = if status == Status::Verified {
        theme::SPEAKING
    } else {
        MUTED
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

    let name_left = rect.left() + NAME_LEFT;
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

/// The open person's section.
fn details(ui: &mut Ui, person: &Participant, demo: bool, state: &mut Verifying) -> Option<Step> {
    let margin = Margin {
        left: 6,
        right: 6,
        top: 10,
        bottom: 14,
    };
    egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if person.verified {
                let forget = pill(ui, None, "Forget", Tone::Quiet, ui.available_width())
                    .on_hover_text("Stop treating this device as verified.");
                return forget.clicked().then_some(Step::Forgot);
            }
            if state.by_hand {
                by_hand(ui, person, state)
            } else {
                by_paste(ui, person, demo, state)
            }
        })
        .inner
}

/// The usual way: swap codes through another app, and Veil compares them.
fn by_paste(
    ui: &mut Ui,
    person: &Participant,
    demo: bool,
    state: &mut Verifying,
) -> Option<Step> {
    let mut step = None;
    let name = first_name(&person.name);
    ui.label(RichText::new(format!("Swap codes with {name}")).strong());
    ui.label(
        RichText::new("Use an app you already trust, not this call's chat.")
            .small()
            .color(MUTED),
    )
    .on_hover_text("Someone between you could change a code sent through this call.");
    ui.add_space(2.0);
    let width = ui.available_width();
    if pill(ui, Some(Icon::Copy), "Copy my code", Tone::Quiet, width).clicked() {
        ui.ctx().copy_text(share_code(&person.safety, person.side));
        step = Some(Step::Copied);
    }
    ui.add(
        TextEdit::singleline(&mut state.pasted)
            .hint_text(format!("Paste {name}'s code"))
            .char_limit(PASTE_LIMIT)
            .desired_width(width),
    );
    if !state.pasted.trim().is_empty() {
        let check = check_code(&person.safety, person.side, &state.pasted);
        if check == CodeCheck::Match {
            step = Some(Step::Confirmed);
        }
        if let Some((text, tip, colour)) = paste_message(check) {
            ui.label(RichText::new(text).small().color(colour))
                .on_hover_text(tip);
        }
    }
    ui.horizontal(|ui| {
        if quiet_link(ui, "Compare by hand", MUTED)
            .on_hover_text("Read the full number together, in person or by phone.")
            .clicked()
        {
            state.by_hand = true;
        }
        if demo
            && quiet_link(ui, "Fill demo code", theme::WARN)
                .on_hover_text("Pretends the other person sent you their code.")
                .clicked()
        {
            state.pasted = share_code(&person.safety, person.side.other());
        }
    });
    step
}

/// The fallback: read all 60 digits together.
fn by_hand(ui: &mut Ui, person: &Participant, state: &mut Verifying) -> Option<Step> {
    ui.label(
        RichText::new("Read this together, in person or by phone.")
            .small()
            .color(MUTED),
    );
    let groups: Vec<&str> = person.safety.split(' ').collect();
    for line in groups.chunks(4) {
        ui.label(RichText::new(line.join("  ")).monospace().size(13.0));
    }
    ui.add_space(2.0);
    let matched = pill(ui, None, "They match", Tone::Quiet, ui.available_width())
        .on_hover_text("Remembers this device for future calls.");
    if quiet_link(ui, "Swap codes instead", MUTED).clicked() {
        state.by_hand = false;
    }
    matched.clicked().then_some(Step::Confirmed)
}

fn quiet_link(ui: &mut Ui, text: &str, colour: egui::Color32) -> Response {
    ui.add(Button::new(RichText::new(text).small().color(colour)).frame(false))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
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
        let open = toggled(None, 1);
        assert_eq!(open.as_ref().map(|o| o.id), Some(1));
        let open = toggled(open, 2);
        assert_eq!(open.as_ref().map(|o| o.id), Some(2));
        assert!(toggled(open, 2).is_none());
    }

    #[test]
    fn switching_person_clears_the_pasted_code() {
        let mut open = Verifying::new(1);
        open.pasted = String::from("VEIL-A 1");
        let next = toggled(Some(open), 2).expect("second person opens");
        assert!(next.pasted.is_empty());
    }

    #[test]
    fn every_failed_paste_says_why() {
        assert!(paste_message(CodeCheck::Match).is_none());
        for check in [CodeCheck::OwnCode, CodeCheck::Different, CodeCheck::NotACode] {
            assert!(paste_message(check).is_some());
        }
    }

    #[test]
    fn relayed_calls_say_the_relay_cannot_read_them() {
        for path in [LinkPath::Relayed, LinkPath::Forwarder] {
            assert!(connection_hint(path).contains("cannot read"));
        }
    }

    #[test]
    fn first_names_fall_back_to_the_whole_name() {
        assert_eq!(first_name("Amara Okafor"), "Amara");
        assert_eq!(first_name("   "), "   ");
    }
}
