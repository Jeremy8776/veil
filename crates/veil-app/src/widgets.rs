//! Veil's own controls: call-bar buttons, pill buttons, avatars, video tiles, the meter.
//!
//! Purpose: every screen draws with these, so a control looks and behaves the same
//! everywhere. Each control is painted by hand for the rounded, quiet look, and reports a
//! label to screen readers (AccessKit) and as a tooltip, because many show only an icon.
//!
//! Invariants:
//! - Every clickable control has a text label for assistive technology.
//! - Colours come from [`crate::theme`] only.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Response, Role, Sense, Stroke, StrokeKind,
    TextureId, Ui, WidgetInfo, pos2, vec2,
};
use veil_media::{Blur, Filter};

use crate::icons::Icon;
use crate::media::LocalMedia;
use crate::theme::{self, ACCENT, DANGER, MUTED, RADIUS, RAISED, RAISED_HOVER, TEXT};

/// How a call-bar button looks.
#[derive(Clone, Copy, Default)]
pub struct ControlState {
    /// The thing it controls is off (muted, camera off): slashed icon.
    pub off: bool,
    /// Its panel is open or its mode is on: accent underline.
    pub selected: bool,
    /// A count to show in a badge, such as people waiting.
    pub badge: usize,
    /// The left half of a split button: square right corners and no gap after it, so the
    /// menu arrow drawn next joins on.
    pub split: bool,
}

/// The corners of a call-bar button, square on the right when it is split.
pub fn control_corners(split: bool) -> CornerRadius {
    if split {
        CornerRadius {
            nw: RADIUS,
            sw: RADIUS,
            ne: 0,
            se: 0,
        }
    } else {
        CornerRadius::same(RADIUS)
    }
}

/// A square call-bar button: icon above a short label.
pub fn control(ui: &mut Ui, icon: Icon, label: &str, hint: &str, state: ControlState) -> Response {
    // egui adds the item gap after a widget, so a split button drops it: the arrow drawn
    // next joins on.
    let gap = ui.spacing().item_spacing.x;
    if state.split {
        ui.spacing_mut().item_spacing.x = 0.0;
    }
    let (rect, response) = ui.allocate_exact_size(vec2(68.0, 58.0), Sense::click());
    ui.spacing_mut().item_spacing.x = gap;
    let response = response.on_hover_text(hint);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, state.selected, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if response.hovered() {
            RAISED_HOVER
        } else {
            RAISED
        };
        let corners = control_corners(state.split);
        painter.rect_filled(rect, corners, fill);
        if response.has_focus() {
            painter.rect_stroke(rect, corners, Stroke::new(2.0, ACCENT), StrokeKind::Inside);
        }
        let colour = if state.off { theme::DANGER_HOVER } else { TEXT };
        let center = pos2(rect.center().x, rect.top() + 22.0);
        if state.off {
            icon.paint_off(painter, center, 20.0, colour, fill);
        } else {
            icon.paint(painter, center, 20.0, colour);
        }
        painter.text(
            pos2(rect.center().x, rect.bottom() - 13.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.5),
            if state.off { colour } else { MUTED },
        );
        if state.selected {
            let bar =
                Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 2.0), vec2(22.0, 3.0));
            painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
        }
        if state.badge > 0 {
            badge(
                painter,
                pos2(rect.right() - 14.0, rect.top() + 10.0),
                state.badge,
            );
        }
    }
    response
}

/// A small red count, as on the People button when someone waits.
pub fn badge(painter: &egui::Painter, center: egui::Pos2, count: usize) {
    let text = if count > 9 {
        String::from("9+")
    } else {
        count.to_string()
    };
    painter.circle_filled(center, 8.5, DANGER);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(10.5),
        Color32::WHITE,
    );
}

/// What a pill button looks like.
#[derive(Clone, Copy)]
pub enum Tone {
    Primary,
    Danger,
    Quiet,
}

/// A rounded button with an optional icon, `width` wide (or fitted to its text for 0).
pub fn pill(ui: &mut Ui, icon: Option<Icon>, label: &str, tone: Tone, width: f32) -> Response {
    let font = theme::bold(14.0);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), TEXT)
        .size()
        .x;
    let icon_w = if icon.is_some() { 26.0 } else { 0.0 };
    let w = width.max(text_w + icon_w + 32.0);
    let (rect, response) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let (fill, fg) = match tone {
            Tone::Primary => (
                if hovered { theme::ACCENT_HOVER } else { ACCENT },
                Color32::WHITE,
            ),
            Tone::Danger => (
                if hovered { theme::DANGER_HOVER } else { DANGER },
                Color32::WHITE,
            ),
            Tone::Quiet => (if hovered { RAISED_HOVER } else { RAISED }, TEXT),
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                RADIUS + 2,
                Stroke::new(2.0, TEXT),
                StrokeKind::Outside,
            );
        }
        let content_w = text_w + icon_w;
        let left = rect.center().x - content_w / 2.0;
        if let Some(icon) = icon {
            icon.paint(painter, pos2(left + 9.0, rect.center().y), 16.0, fg);
        }
        painter.text(
            pos2(left + icon_w, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            fg,
        );
    }
    response
}

/// A small icon-only button with a tooltip and an accessible label.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let response = response.on_hover_text(label);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() || response.has_focus() {
            painter.rect_filled(rect, CornerRadius::same(6), RAISED_HOVER);
        }
        icon.paint(painter, rect.center(), 15.0, TEXT);
    }
    response
}

/// A round avatar with initials, coloured from the name.
pub fn avatar(painter: &egui::Painter, center: egui::Pos2, radius: f32, name: &str) {
    painter.circle_filled(center, radius, theme::avatar_colour(name));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        theme::initials(name),
        theme::bold(radius * 0.8),
        Color32::WHITE,
    );
}

/// What one video tile shows.
pub struct Tile<'a> {
    pub name: &'a str,
    /// The picture to draw and its size in pixels.
    pub video: Option<(TextureId, [usize; 2])>,
    pub muted: bool,
    pub speaking: bool,
    pub hand_raised: bool,
    pub is_you: bool,
}

/// Paints a tile: the picture or an avatar, the name tag, mute and hand marks, and a ring
/// while speaking.
pub fn tile(ui: &Ui, rect: Rect, tile: &Tile<'_>) {
    let painter = ui.painter_at(rect.expand(3.0));
    let radius = CornerRadius::same(RADIUS);
    painter.rect_filled(rect, radius, theme::TILE);
    match tile.video {
        Some((texture, size)) => {
            let uv = cover_uv(size, rect.size());
            painter.image(texture, rect, uv, Color32::WHITE);
            // Rounds the picture's corners by covering them in the page colour.
            painter.rect_stroke(
                rect.expand(1.0),
                RADIUS + 1,
                Stroke::new(3.0, theme::PAGE),
                StrokeKind::Inside,
            );
        }
        None => {
            let r = (rect.height() * 0.2).clamp(14.0, 64.0);
            avatar(&painter, rect.center(), r, tile.name);
        }
    }
    if tile.speaking {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(2.5, theme::SPEAKING),
            StrokeKind::Inside,
        );
    }
    let small = rect.height() < 120.0;
    let label = if tile.is_you {
        format!("{} (You)", tile.name)
    } else {
        tile.name.to_string()
    };
    let font = FontId::proportional(if small { 11.0 } else { 13.0 });
    let galley = painter.layout_no_wrap(label, font, TEXT);
    let mut tag_w = galley.size().x + 16.0;
    if tile.muted {
        tag_w += 20.0;
    }
    let tag = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.bottom() - 8.0 - 24.0),
        vec2(tag_w.min(rect.width() - 16.0), 24.0),
    );
    painter.rect_filled(tag, CornerRadius::same(6), Color32::from_black_alpha(160));
    let mut x = tag.left() + 8.0;
    if tile.muted {
        Icon::MicOff.paint(
            &painter,
            pos2(x + 7.0, tag.center().y),
            12.0,
            theme::DANGER_HOVER,
        );
        x += 20.0;
    }
    painter.galley(
        pos2(x, tag.center().y - galley.size().y / 2.0),
        galley,
        TEXT,
    );
    if tile.hand_raised {
        let mark = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 8.0), vec2(30.0, 26.0));
        painter.rect_filled(mark, CornerRadius::same(6), theme::WARN);
        painter.text(
            mark.center(),
            Align2::CENTER_CENTER,
            "\u{270B}",
            FontId::proportional(15.0),
            theme::PAGE,
        );
    }
}

/// The part of a `size` picture that fills a tile of `tile` size without stretching, centred.
pub fn cover_uv(size: [usize; 2], tile: egui::Vec2) -> Rect {
    let picture = size[0] as f32 / size[1].max(1) as f32;
    let target = tile.x / tile.y.max(1.0);
    let (w, h) = if picture > target {
        (target / picture, 1.0)
    } else {
        (1.0, picture / target)
    };
    let (x0, y0) = ((1.0 - w) / 2.0, (1.0 - h) / 2.0);
    Rect::from_min_max(pos2(x0, y0), pos2(x0 + w, y0 + h))
}

/// A row of segments lighting up with the microphone level, 0 to 1.
pub fn level_meter(ui: &mut Ui, level: f32, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, true, "Microphone level"));
    let segments = 16;
    let gap = 3.0;
    let w = (rect.width() - gap * (segments - 1) as f32) / segments as f32;
    let lit = (level.clamp(0.0, 1.0) * segments as f32).round() as usize;
    for i in 0..segments {
        let x = rect.left() + i as f32 * (w + gap);
        let seg = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
        let colour = if i < lit {
            theme::SPEAKING
        } else {
            theme::LINE
        };
        ui.painter().rect_filled(seg, CornerRadius::same(2), colour);
    }
    response
}

/// A rounded card on the page, for home and pre-join content.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(CornerRadius::same(12))
        .stroke(Stroke::new(1.0, theme::LINE))
        .inner_margin(egui::Margin::same(20))
}

/// The small muted icon and name above a drop-down.
pub fn picker_label(ui: &mut Ui, icon: Icon, kind: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        icon.paint(ui.painter(), rect.center(), 15.0, MUTED);
        ui.label(egui::RichText::new(kind).small().color(MUTED));
    });
}

/// A labelled drop-down of devices, shared by the pre-join screen and the in-call menus. Returns the new choice when it changed (`Some(None)` for
/// the default).
pub fn device_picker(
    ui: &mut Ui,
    icon: Icon,
    kind: &str,
    list: &[crate::devices::Device],
    chosen: Option<&str>,
    default: &str,
) -> Option<Option<String>> {
    let mut picked = chosen.map(str::to_string);
    picker_label(ui, icon, kind);
    egui::ComboBox::from_id_salt(kind)
        .width(280.0)
        .selected_text(crate::devices::label(list, chosen, default))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut picked, None, default);
            for device in list {
                ui.selectable_value(&mut picked, Some(device.id.clone()), &device.name);
            }
        });
    (picked.as_deref() != chosen).then_some(picked)
}

/// A device error, in short, under its picker.
pub fn device_error(ui: &mut Ui, error: Option<String>) {
    if let Some(error) = error {
        ui.label(
            egui::RichText::new(error)
                .small()
                .color(theme::DANGER_HOVER),
        );
    }
}

/// The Filter drop-down, shared by the pre-join screen and the in-call Effects popup.
pub fn filter_picker(ui: &mut Ui, media: &LocalMedia) {
    picker_label(ui, Icon::Effects, "Filter");
    let current = media.filter();
    let mut picked = current;
    egui::ComboBox::from_id_salt("Filter")
        .width(280.0)
        .selected_text(filter_label(current))
        .show_ui(ui, |ui| {
            for filter in Filter::ALL {
                ui.selectable_value(&mut picked, filter, filter_label(filter));
            }
        });
    if picked != current {
        media.set_filter(picked);
    }
}

fn filter_label(filter: Filter) -> &'static str {
    match filter {
        Filter::None => "None",
        Filter::Warm => "Warm",
        Filter::Cool => "Cool",
        Filter::BlackWhite => "Black & white",
        Filter::Bright => "Bright",
    }
}

/// The Background blur drop-down, beside the Filter one, with the notice if blur turned itself off.
pub fn blur_picker(ui: &mut Ui, media: &LocalMedia) {
    picker_label(ui, Icon::Effects, "Background blur");
    let current = media.blur();
    let mut picked = current;
    egui::ComboBox::from_id_salt("Background blur")
        .width(280.0)
        .selected_text(blur_label(current))
        .show_ui(ui, |ui| {
            for blur in Blur::ALL {
                ui.selectable_value(&mut picked, blur, blur_label(blur));
            }
        })
        .response
        .on_hover_text(
            "Blur is not a privacy guarantee: edges and hair can show what is behind you.",
        );
    if picked != current {
        media.set_blur(picked);
    }
    if let Some(notice) = media.blur_notice() {
        ui.label(
            egui::RichText::new(notice)
                .small()
                .color(theme::DANGER_HOVER),
        )
        .on_hover_text("Turn the camera on again when fewer apps are running, or turn blur off.");
    }
}

fn blur_label(blur: Blur) -> &'static str {
    match blur {
        Blur::Off => "Off",
        Blur::Light => "Light",
        Blur::Strong => "Strong",
    }
}

/// A row of widgets centred in the available width. egui lays a row out from the left, so the
/// row's width from the last frame sets the left margin; a row that changes width is off
/// centre for one frame, and asks for that frame.
pub fn centred_row<R>(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let id = ui.make_persistent_id(id);
    let last: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let available = ui.available_width();
    let (inner, width) = ui
        .horizontal(|ui| {
            ui.add_space(((available - last) / 2.0).max(0.0));
            let row = ui.horizontal(add);
            (row.inner, row.response.rect.width())
        })
        .inner;
    if (width - last).abs() > 0.5 {
        ui.ctx().data_mut(|d| d.insert_temp(id, width));
        ui.ctx().request_repaint();
    }
    inner
}
