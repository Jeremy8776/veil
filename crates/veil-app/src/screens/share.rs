//! Screen sharing on the call screen: the picker, and someone's shared screen on the stage.
//!
//! Purpose: the Share button opens [`SharePicker`]: choose a screen or window, see it, and
//! optionally drag over the picture to share only that part. A shared screen is drawn whole
//! ([`stage`]), never cropped to fit, because its text has to stay readable.
//!
//! Invariants:
//! - Nothing goes to the call until Share is pressed in the picker.
//! - The crop is held as fractions of the source (`veil_media::screen::Crop`).

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Modal, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextWrapMode, TextureHandle, TextureId, TextureOptions, pos2, vec2,
};
use veil_media::screen::{Crop, ScreenChoice, ScreenSource};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card, device_error, pill};

const LIST_W: f32 = 230.0;
/// A drag smaller than this fraction of the picture counts as a click.
const MIN_CROP: f32 = 0.03;
const WHOLE: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

pub struct SharePicker {
    selected: Option<ScreenSource>,
    /// The chosen part of the picture, as fractions of it.
    crop: Option<Rect>,
    drag_from: Option<Pos2>,
    preview: Option<TextureHandle>,
}

impl SharePicker {
    pub fn open(app: &mut VeilApp) -> Self {
        app.share.list();
        Self {
            selected: None,
            crop: None,
            drag_from: None,
            preview: None,
        }
    }

    fn select(&mut self, app: &mut VeilApp, source: ScreenSource) {
        self.selected = Some(source);
        self.crop = None;
        self.drag_from = None;
        self.preview = None;
        app.share.start_preview(source);
    }

    /// Draws the picker. Returns false once it is closed.
    pub fn show(&mut self, app: &mut VeilApp, ui: &egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        self.upload_preview(app, &ctx);
        let choices = app.share.choices();
        if self.selected.is_none()
            && let Some(first) = choices.as_ref().and_then(|list| list.first())
        {
            self.select(app, first.source);
        }
        let screen = ctx.content_rect().size();
        let width = (screen.x - 80.0).clamp(520.0, 900.0);
        let mut keep = true;
        let mut share = false;
        let modal = Modal::new(ui.scope_id().with("share-picker"))
            .frame(card())
            .show(&ctx, |ui| {
                ui.set_width(width);
                ui.label(RichText::new("Share").font(theme::bold(18.0)));
                ui.add_space(10.0);
                let picture_w = width - LIST_W - 16.0;
                let picture_h = (picture_w * 9.0 / 16.0).min(screen.y - 220.0).max(160.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(LIST_W);
                        ui.set_height(picture_h);
                        if let Some(source) = self.list(ui, choices.as_deref(), picture_h) {
                            self.select(app, source);
                        }
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| self.picture(ui, vec2(picture_w, picture_h)));
                });
                ui.add_space(6.0);
                let hint = if self.crop.is_some() {
                    "Click the picture to share all of it"
                } else {
                    "Drag on the picture to share part of it"
                };
                ui.label(RichText::new(hint).small().color(MUTED));
                device_error(ui, app.share.error());
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ready = self.selected.is_some();
                    ui.add_enabled_ui(ready, |ui| {
                        share = pill(ui, Some(Icon::Share), "Share", Tone::Primary, 120.0).clicked();
                    });
                    if pill(ui, None, "Cancel", Tone::Quiet, 100.0).clicked() {
                        keep = false;
                    }
                });
            });
        if let (true, Some(source)) = (share, self.selected) {
            app.share.start(source, self.crop.map(to_crop));
            return false;
        }
        if !keep || modal.should_close() {
            app.share.stop_preview();
            return false;
        }
        true
    }

    fn upload_preview(&mut self, app: &VeilApp, ctx: &egui::Context) {
        let Some(picture) = app.share.take_preview() else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [picture.width, picture.height],
            &picture.pixels,
        );
        match &mut self.preview {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => {
                let options = TextureOptions::LINEAR;
                self.preview = Some(ctx.load_texture("share-preview", image, options));
            }
        }
    }

    /// The list of screens and windows. Returns a newly chosen source.
    fn list(
        &self,
        ui: &mut egui::Ui,
        choices: Option<&[ScreenChoice]>,
        height: f32,
    ) -> Option<ScreenSource> {
        let Some(choices) = choices else {
            ui.label(RichText::new("Looking…").color(MUTED));
            return None;
        };
        let mut chosen = None;
        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
        ScrollArea::vertical()
            .id_salt("share-sources")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let windows = |c: &&ScreenChoice| matches!(c.source, ScreenSource::Window(_));
                let (apps, screens): (Vec<_>, Vec<_>) = choices.iter().partition(windows);
                for (title, group) in [("Screens", screens), ("Windows", apps)] {
                    if group.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(title).small().color(MUTED));
                    for choice in group {
                        let on = self.selected == Some(choice.source);
                        let row = ui.selectable_label(on, &choice.label);
                        if row.on_hover_text(&choice.label).clicked() && !on {
                            chosen = Some(choice.source);
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        chosen
    }

    /// The chosen source, with its crop drawn over it and dragged out on it.
    fn picture(&mut self, ui: &mut egui::Ui, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(8), theme::TILE);
        let Some(texture) = &self.preview else { return };
        let image = contain(texture.size(), rect);
        painter.image(texture.id(), image, WHOLE, Color32::WHITE);
        let to_uv = |at: Pos2| {
            let p = (at - image.min) / image.size();
            pos2(p.x.clamp(0.0, 1.0), p.y.clamp(0.0, 1.0))
        };
        let pointer = response.interact_pointer_pos().map(to_uv);
        if response.drag_started() {
            self.drag_from = pointer;
            self.crop = None;
        }
        if let (true, Some(from), Some(to)) = (response.dragged(), self.drag_from, pointer) {
            self.crop = Some(Rect::from_two_pos(from, to));
        }
        if response.drag_stopped() {
            self.drag_from = None;
            self.crop = self.crop.filter(|c| c.width().min(c.height()) >= MIN_CROP);
        }
        if response.clicked() {
            self.crop = None;
        }
        let Some(crop) = self.crop else { return };
        let at = |uv: Pos2| image.min + uv.to_vec2() * image.size();
        let part = Rect::from_min_max(at(crop.min), at(crop.max));
        let shade = Color32::from_black_alpha(150);
        for dark in [
            Rect::from_min_max(image.min, pos2(image.right(), part.top())),
            Rect::from_min_max(pos2(image.left(), part.bottom()), image.max),
            Rect::from_min_max(pos2(image.left(), part.top()), part.left_bottom()),
            Rect::from_min_max(part.right_top(), pos2(image.right(), part.bottom())),
        ] {
            painter.rect_filled(dark, 0.0, shade);
        }
        let line = Stroke::new(2.0, theme::ACCENT);
        painter.rect_stroke(part, 0.0, line, egui::StrokeKind::Outside);
    }
}

fn to_crop(rect: Rect) -> Crop {
    Crop {
        left: rect.left(),
        top: rect.top(),
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

/// The largest rectangle of a `size` picture's shape that fits in `area`, centred.
pub fn contain(size: [usize; 2], area: Rect) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (area.width() / w).min(area.height() / h);
    Rect::from_center_size(area.center(), vec2(w * scale, h * scale))
}

/// Someone's shared screen, whole, with their name.
pub fn stage(ui: &egui::Ui, rect: Rect, (texture, size): (TextureId, [usize; 2]), name: &str) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), Color32::BLACK);
    painter.image(texture, contain(size, rect), WHOLE, Color32::WHITE);
    let text = format!("{name} is sharing");
    let font = FontId::proportional(13.0);
    let galley = painter.layout_no_wrap(text, font, Color32::WHITE);
    let tag = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.bottom() - 32.0),
        galley.size() + vec2(16.0, 8.0),
    );
    painter.rect_filled(tag, CornerRadius::same(6), Color32::from_black_alpha(190));
    let at = Align2::LEFT_CENTER.pos_in_rect(&tag.shrink2(vec2(8.0, 0.0)));
    painter.galley(at - vec2(0.0, galley.size().y / 2.0), galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_whole_and_centred() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0));
        let wide = contain([1_920, 1_080], area);
        assert_eq!(wide.width(), 200.0);
        assert!((wide.height() - 112.5).abs() < 0.01);
        assert_eq!(wide.center(), area.center());
        let tall = contain([100, 400], area);
        assert_eq!((tall.width(), tall.height()), (50.0, 200.0));
    }

    #[test]
    fn a_crop_keeps_its_fractions() {
        let rect = Rect::from_min_max(pos2(0.25, 0.5), pos2(0.75, 1.0));
        let crop = to_crop(rect);
        assert_eq!((crop.left, crop.top, crop.right, crop.bottom), (0.25, 0.5, 0.75, 1.0));
    }
}
