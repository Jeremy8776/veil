//! Phone controls. Touch targets stay 48 points while labels use the shared type scale.
use crate::{icons::Icon, theme, widgets::Tone};
use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Rect, Response, Role, Sense, Stroke, StrokeKind,
    WidgetInfo, pos2, vec2,
};

pub const TARGET: f32 = 48.0;
pub const PRIMARY: Color32 = Color32::from_rgb(0x62, 0x5C, 0xDB);
pub const SELECTED: Color32 = Color32::from_rgb(0x32, 0x2E, 0x53);
pub const ON_SELECTED: Color32 = Color32::from_rgb(0xDE, 0xDA, 0xFF);
/// Accent text on the dark page, for links and inline actions. Lighter than PRIMARY so it
/// reads as text at 13 to 16 points.
pub const LINK: Color32 = Color32::from_rgb(0x9C, 0x98, 0xFF);

pub fn font(ui: &egui::Ui, size: f32, bold: bool) -> FontId {
    let scale = egui::TextStyle::Body.resolve(ui.style()).size / 16.0;
    FontId::new(
        size * scale,
        if bold {
            FontFamily::Name("bold".into())
        } else {
            FontFamily::Proportional
        },
    )
}

pub fn label(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    size: f32,
    bold: bool,
    color: Color32,
) -> Response {
    ui.label(
        egui::RichText::new(text.into())
            .font(font(ui, size, bold))
            .color(color),
    )
}

pub fn action(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    text: &str,
    tone: Tone,
    width: f32,
) -> Response {
    let width = width.min(ui.available_width()).max(1.0);
    let icon_space = if icon.is_some() { 28.0 } else { 0.0 };
    let galley = ui.painter().layout(
        text.to_owned(),
        font(ui, 14.0, true),
        theme::TEXT,
        (width - 32.0 - icon_space).max(1.0),
    );
    let height = TARGET.max(galley.size().y + 20.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), text));
    let (fill, ink) = match tone {
        Tone::Primary => (
            if response.hovered() {
                theme::ACCENT
            } else {
                PRIMARY
            },
            Color32::WHITE,
        ),
        Tone::Danger => (theme::DANGER, Color32::WHITE),
        Tone::Quiet => (
            if response.hovered() {
                theme::RAISED_HOVER
            } else {
                theme::SURFACE
            },
            theme::TEXT,
        ),
    };
    let (fill, ink) = if ui.is_enabled() {
        (fill, ink)
    } else {
        (theme::SURFACE, theme::MUTED)
    };
    ui.painter().rect_filled(rect, 24, fill);
    if matches!(tone, Tone::Quiet) {
        ui.painter()
            .rect_stroke(rect, 24, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
    }
    focus(ui, &response, rect, 24);
    let left = rect.center().x - (galley.size().x + icon_space) * 0.5;
    if let Some(icon) = icon {
        icon.paint(ui.painter(), pos2(left + 10.0, rect.center().y), 20.0, ink);
    }
    ui.painter().galley_with_override_text_color(
        pos2(left + icon_space, rect.center().y - galley.size().y * 0.5),
        galley,
        ink,
    );
    response
}

pub fn icon(ui: &mut egui::Ui, icon: Icon, text: &str, selected: bool) -> Response {
    glyph(ui, text, selected, |painter, rect, ink| {
        icon.paint(painter, rect.center(), 22.0, ink);
    })
}

pub fn back(ui: &mut egui::Ui) -> Response {
    glyph(ui, "Back", false, |painter, rect, ink| {
        let c = rect.center();
        painter.line_segment(
            [c + vec2(7.0, 0.0), c - vec2(8.0, 0.0)],
            Stroke::new(2.0, ink),
        );
        painter.line_segment(
            [c - vec2(8.0, 0.0), c + vec2(-1.0, -7.0)],
            Stroke::new(2.0, ink),
        );
        painter.line_segment(
            [c - vec2(8.0, 0.0), c + vec2(-1.0, 7.0)],
            Stroke::new(2.0, ink),
        );
    })
}

/// An inline forward arrow, tinted like a link, for Join. Grey until it can be used.
pub fn go(ui: &mut egui::Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(TARGET, TARGET), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), "Join"));
    let ink = if ui.is_enabled() { LINK } else { theme::MUTED };
    if ui.is_enabled() && response.hovered() {
        ui.painter().rect_filled(rect, 24, theme::RAISED);
    }
    let c = rect.center();
    let stroke = Stroke::new(2.5, ink);
    ui.painter()
        .line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], stroke);
    ui.painter()
        .line_segment([c + vec2(8.0, 0.0), c + vec2(1.0, -7.0)], stroke);
    ui.painter()
        .line_segment([c + vec2(8.0, 0.0), c + vec2(1.0, 7.0)], stroke);
    focus(ui, &response, rect, 24);
    response.on_hover_text("Join")
}

/// A quiet text button for a secondary choice. It keeps a 48 point touch height.
pub fn link(ui: &mut egui::Ui, icon: Option<Icon>, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        font(ui, 16.0, false),
        LINK,
    );
    let icon_space = if icon.is_some() { 26.0 } else { 0.0 };
    let width = (galley.size().x + icon_space + 24.0).min(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(vec2(width, TARGET), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), text));
    let ink = if ui.is_enabled() {
        LINK
    } else {
        theme::MUTED
    };
    if response.hovered() {
        ui.painter().rect_filled(rect, 24, theme::RAISED);
    }
    focus(ui, &response, rect, 24);
    let left = rect.left() + 12.0;
    if let Some(icon) = icon {
        icon.paint(ui.painter(), pos2(left + 9.0, rect.center().y), 18.0, ink);
    }
    ui.painter().galley_with_override_text_color(
        pos2(left + icon_space, rect.center().y - galley.size().y * 0.5),
        galley,
        ink,
    );
    response
}

pub fn more(ui: &mut egui::Ui) -> Response {
    glyph(ui, "Call options", false, |painter, rect, ink| {
        for y in [-6.0, 0.0, 6.0] {
            painter.circle_filled(rect.center() + vec2(0.0, y), 1.8, ink);
        }
    })
}

fn glyph(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    paint: impl FnOnce(&egui::Painter, Rect, Color32),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(TARGET, TARGET), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, ui.is_enabled(), selected, text));
    let ink = if !ui.is_enabled() {
        theme::MUTED
    } else if selected {
        ON_SELECTED
    } else {
        theme::TEXT
    };
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, 24, if selected { SELECTED } else { theme::RAISED });
    }
    focus(ui, &response, rect, 24);
    paint(ui.painter(), rect, ink);
    response.on_hover_text(text)
}

pub fn badge(ui: &egui::Ui, response: &Response, count: usize) {
    if count == 0 {
        return;
    }
    let center = response.rect.right_top() + vec2(-7.0, 7.0);
    ui.painter().circle_filled(center, 9.0, PRIMARY);
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        if count > 99 {
            "99+".into()
        } else {
            count.to_string()
        },
        FontId::proportional(10.0),
        Color32::WHITE,
    );
}

pub fn call_control(
    ui: &mut egui::Ui,
    icon: Icon,
    text: &str,
    off: bool,
    danger: bool,
) -> Response {
    let width = ui.available_width();
    let text_galley = ui.painter().layout(
        text.to_owned(),
        font(ui, 12.0, false),
        theme::TEXT,
        width.max(1.0),
    );
    let height = TARGET + 6.0 + text_galley.size().y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), text));
    let circle = Rect::from_center_size(
        pos2(rect.center().x, rect.top() + TARGET * 0.5),
        vec2(TARGET, TARGET),
    );
    let fill = if danger || off {
        theme::DANGER
    } else if response.hovered() {
        theme::RAISED_HOVER
    } else {
        theme::RAISED
    };
    let ink = if ui.is_enabled() {
        theme::TEXT
    } else {
        theme::MUTED
    };
    ui.painter().circle_filled(circle.center(), 24.0, fill);
    if off {
        icon.paint_off(ui.painter(), circle.center(), 22.0, ink, fill);
    } else {
        icon.paint(ui.painter(), circle.center(), 22.0, ink);
    }
    focus(ui, &response, circle, 24);
    ui.painter().galley_with_override_text_color(
        pos2(
            rect.center().x - text_galley.size().x * 0.5,
            circle.bottom() + 6.0,
        ),
        text_galley,
        ink,
    );
    response
}

pub fn switch(ui: &mut egui::Ui, value: &mut bool, title: &str, detail: &str) -> Response {
    let width = ui.available_width();
    let text_width = (width - 72.0).max(1.0);
    let title = ui
        .painter()
        .layout(title.into(), font(ui, 16.0, false), theme::TEXT, text_width);
    let detail_galley = ui.painter().layout(
        detail.into(),
        font(ui, 13.0, false),
        theme::MUTED,
        text_width,
    );
    let gap = if detail.is_empty() { 0.0 } else { 4.0 };
    let height = TARGET.max(title.size().y + detail_galley.size().y + gap + 20.0);
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response
        .widget_info(|| WidgetInfo::selected(Role::Switch, ui.is_enabled(), *value, title.text()));
    let color = if ui.is_enabled() {
        theme::TEXT
    } else {
        theme::MUTED
    };
    let top = rect.center().y - (title.size().y + detail_galley.size().y + gap) * 0.5;
    let detail_y = top + title.size().y + gap;
    ui.painter()
        .galley_with_override_text_color(pos2(rect.left(), top), title, color);
    if !detail.is_empty() {
        ui.painter()
            .galley(pos2(rect.left(), detail_y), detail_galley, theme::MUTED);
    }
    let track =
        Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), vec2(52.0, 30.0));
    ui.painter().rect_filled(
        track,
        15,
        if *value && ui.is_enabled() {
            PRIMARY
        } else {
            theme::RAISED
        },
    );
    ui.painter().rect_stroke(
        track,
        15,
        Stroke::new(1.0, if *value { PRIMARY } else { theme::MUTED }),
        StrokeKind::Inside,
    );
    ui.painter().circle_filled(
        pos2(
            if *value {
                track.right() - 15.0
            } else {
                track.left() + 15.0
            },
            track.center().y,
        ),
        if *value { 11.0 } else { 8.0 },
        color,
    );
    focus(ui, &response, rect, 8);
    response
}

fn focus(ui: &egui::Ui, response: &Response, rect: Rect, radius: u8) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            radius,
            Stroke::new(2.0, theme::TEXT),
            StrokeKind::Outside,
        );
    }
}
