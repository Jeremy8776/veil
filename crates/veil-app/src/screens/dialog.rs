//! The desktop's house dialog and button pair, so small windows such as the connection report
//! and the invite QR look like the rest of Veil rather than raw egui windows.
//!
//! A dialog is a card over the dimmed page, as in a Mac sheet: a close button at the top left
//! beside a centred title, then one centred column of a fixed width that scrolls when the
//! window is short. Escape and a click outside close it.
//!
//! Invariants:
//! - The column never grows wider than the window allows ([`width`]), so the card stays inside
//!   the window at any size.
//! - A [`pair`] fills its row exactly: two buttons of one width either side of the centre line,
//!   the main action on the right.

use eframe::egui::{self, Align, Layout, Modal, Response, RichText, Sense, Ui, vec2};

use crate::icons::Icon;
use crate::theme::{self, GAP};
use crate::widgets::{self, Tone, card, pill};

/// The title row's height, the same as the close button's hit area.
const TITLE_H: f32 = 36.0;
/// Space kept between the card and the window's edge.
const EDGE: f32 = 24.0;
/// The card's inner margin, from `widgets::card`.
const CARD_MARGIN: f32 = 20.0;
/// The narrowest column a dialog uses, even in a tiny window.
const MIN_WIDTH: f32 = 240.0;

/// The column width for a dialog that would like to be `wanted` wide, in a window `window`
/// wide: as wide as asked, less when the card would not fit, never below [`MIN_WIDTH`].
pub fn width(window: f32, wanted: f32) -> f32 {
    wanted
        .min(window - 2.0 * (EDGE + CARD_MARGIN))
        .max(MIN_WIDTH)
}

/// The tallest the scrolling column gets in a window `window` tall.
fn max_height(window: f32) -> f32 {
    (window - 2.0 * (EDGE + CARD_MARGIN) - TITLE_H - GAP).max(120.0)
}

/// Shows a dialog titled `title` with a column `width` wide. Returns false once the person
/// closes it.
pub fn show(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    add: impl FnOnce(&mut Ui),
) -> bool {
    let window = ctx.content_rect().size();
    let modal = Modal::new(egui::Id::unique(("veil-dialog", id)))
        .frame(card())
        .show(ctx, |ui| {
            ui.set_width(width);
            let close = title_row(ui, title, width);
            ui.add_space(GAP);
            egui::ScrollArea::vertical()
                .id_salt(("veil-dialog-scroll", id))
                .max_height(max_height(window.y))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = GAP;
                        add(ui);
                    });
                });
            close
        });
    !(modal.inner || modal.should_close())
}

/// The close button at the left and the title centred over the column. Returns true when the
/// close button was clicked.
fn title_row(ui: &mut Ui, title: &str, width: f32) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(width, TITLE_H), Sense::hover());
    let mut centre = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
    );
    centre.label(RichText::new(title).font(theme::bold(18.0)));
    let corner = egui::Rect::from_min_size(rect.left_top(), vec2(TITLE_H, TITLE_H));
    let mut corner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(corner)
            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
    );
    widgets::icon_button(&mut corner, Icon::Close, "Close (Esc)").clicked()
}

/// One button of a [`pair`].
pub struct Choice<'a> {
    pub icon: Option<Icon>,
    pub label: &'a str,
    pub tone: Tone,
    /// The tooltip; empty for none.
    pub hint: &'a str,
    pub enabled: bool,
}

impl<'a> Choice<'a> {
    pub fn new(icon: Option<Icon>, label: &'a str, tone: Tone) -> Self {
        Self {
            icon,
            label,
            tone,
            hint: "",
            enabled: true,
        }
    }
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Each button's width when two share a row `width` wide.
pub fn half(width: f32) -> f32 {
    ((width - GAP) / 2.0).max(0.0)
}

/// Two buttons of one width filling a row `width` wide. The main action goes on the right.
pub fn pair(ui: &mut Ui, width: f32, left: Choice<'_>, right: Choice<'_>) -> (Response, Response) {
    let each = half(width);
    ui.allocate_ui_with_layout(
        vec2(width, 40.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let a = one(ui, left, each);
            let b = one(ui, right, each);
            (a, b)
        },
    )
    .inner
}

/// A single [`Choice`] `width` wide. When the label and icon would not fit, the icon goes,
/// so the button keeps its width rather than pushing past its row.
pub fn one(ui: &mut Ui, choice: Choice<'_>, width: f32) -> Response {
    let text = ui
        .painter()
        .layout_no_wrap(choice.label.to_owned(), theme::bold(14.0), theme::TEXT)
        .size()
        .x;
    let icon = choice.icon.filter(|_| keeps_icon(text, width));
    let response = ui
        .add_enabled_ui(choice.enabled, |ui| {
            pill(ui, icon, choice.label, choice.tone, width)
        })
        .inner;
    if choice.hint.is_empty() {
        response
    } else {
        response
            .on_hover_text(choice.hint)
            .on_disabled_hover_text(choice.hint)
    }
}

/// Whether a label `text` wide and its icon fit a pill `width` wide, using the pill's own
/// padding (16 each side) and icon slot (26).
fn keeps_icon(text: f32, width: f32) -> bool {
    text + 26.0 + 32.0 <= width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_drops_its_icon_before_it_outgrows_its_row() {
        assert!(keeps_icon(70.0, 128.0));
        assert!(!keeps_icon(80.0, 128.0));
    }

    #[test]
    fn a_wide_window_gets_the_width_asked_for() {
        assert_eq!(width(1600.0, 480.0), 480.0);
    }

    #[test]
    fn a_narrow_window_keeps_the_card_inside_its_edges() {
        let column = width(500.0, 480.0);
        assert_eq!(column + 2.0 * (EDGE + CARD_MARGIN), 500.0);
    }

    #[test]
    fn a_tiny_window_keeps_the_smallest_column() {
        assert_eq!(width(200.0, 480.0), MIN_WIDTH);
    }

    #[test]
    fn a_pair_fills_its_row_exactly() {
        for row in [240.0, 300.0, 441.0] {
            assert!((half(row) * 2.0 + GAP - row).abs() < 0.01);
        }
        assert_eq!(half(4.0), 0.0);
    }

    #[test]
    fn the_column_scrolls_before_the_card_leaves_a_short_window() {
        let window = 600.0;
        assert!(max_height(window) + TITLE_H + GAP + 2.0 * (EDGE + CARD_MARGIN) <= window);
        assert_eq!(max_height(100.0), 120.0);
    }
}
