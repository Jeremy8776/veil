//! Screen sharing on the call screen: the picker and the shared picture.
//!
//! Purpose: the Share button opens [`SharePicker`], laid out like a browser's share dialog: a
//! Screen tab and an Application tab, each a grid of live thumbnails. "Select area" shows the
//! chosen source large, to drag out the part to share. Where the desktop's own chooser picks
//! the source (Wayland), the picker waits for it and then goes straight to "Select area", and
//! "Choose again" opens the chooser again. A shared picture is drawn whole
//! ([`stage`]), never cropped to fit, because its text has to stay readable. The floating bar
//! is `share_bar.rs`.
//!
//! Invariants:
//! - Nothing goes to the call until a source is chosen and Share is pressed.
//! - While you share, the picker changes the source only when you press Switch.
//! - The crop is held as fractions of the source (`veil_media::screen::Crop`).
//! - "Include computer sound" starts off for each new share, and keeps the current choice when
//!   you change what you share. Where this computer cannot capture its sound, it is greyed
//!   out and says why.

use std::collections::HashMap;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Modal, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextureFilter, TextureHandle, TextureId, TextureOptions, Ui, pos2,
    text::LayoutJob, text::TextWrapping, vec2,
};
use veil_media::screen::{self, Crop, ScreenChoice, ScreenSource};

use crate::app::VeilApp;
use crate::convert::Rgba;
use crate::icons::Icon;
use crate::share::SoundSupport;
use crate::theme::{self, MUTED, TEXT};
use crate::widgets::{Tone, card, device_error, pill};

/// A drag smaller than this fraction of the picture counts as a click.
const MIN_CROP: f32 = 0.03;
const WHOLE: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
const CELL_W: f32 = 240.0;
const CELL_GAP: f32 = 12.0;
const CELL_LABEL_H: f32 = 28.0;
/// The space between a thumbnail and the edge of its tile.
const THUMB_PAD: f32 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Screen,
    Application,
}

impl Tab {
    fn holds(self, source: ScreenSource) -> bool {
        matches!(
            (self, source),
            (Self::Screen, ScreenSource::Monitor(_))
                | (Self::Application, ScreenSource::Window(_))
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Choose,
    /// Waiting for the desktop's own chooser.
    Desktop,
    SelectArea,
}

/// What the person did in the picker this frame.
#[derive(Default)]
struct Clicked {
    pick: Option<ScreenSource>,
    share: bool,
    close: bool,
    step: Option<Step>,
}

pub struct SharePicker {
    tab: Tab,
    step: Step,
    selected: Option<ScreenSource>,
    /// The chosen part of the picture, as fractions of it.
    crop: Option<Rect>,
    drag_from: Option<Pos2>,
    preview: Option<TextureHandle>,
    thumbnails: HashMap<ScreenSource, TextureHandle>,
    /// "Include computer sound" is ticked.
    with_sound: bool,
}

fn image_of(picture: &Rgba) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([picture.width, picture.height], &picture.pixels)
}

impl SharePicker {
    pub fn open(app: &mut VeilApp) -> Self {
        let step = if screen::uses_system_chooser() {
            app.share.choose_on_desktop();
            Step::Desktop
        } else {
            app.share.browse();
            Step::Choose
        };
        Self {
            tab: Tab::Screen,
            step,
            selected: None,
            crop: None,
            drag_from: None,
            preview: None,
            thumbnails: HashMap::new(),
            with_sound: app.share.is_sharing() && app.share.sound.state().is_on(),
        }
    }

    fn upload(&mut self, app: &VeilApp, ctx: &egui::Context) {
        let options = TextureOptions::LINEAR;
        if let Some(picture) = app.share.take_preview() {
            let image = image_of(&picture);
            match &mut self.preview {
                Some(texture) => texture.set(image, options),
                None => self.preview = Some(ctx.load_texture("share-preview", image, options)),
            }
        }
        // Mipmaps keep small text from shimmering when a thumbnail is drawn smaller.
        let small = options.with_mipmap_mode(Some(TextureFilter::Linear));
        for (source, picture) in app.share.take_thumbnails() {
            let image = image_of(&picture);
            match self.thumbnails.get_mut(&source) {
                Some(texture) => texture.set(image, small),
                None => {
                    let texture = ctx.load_texture("share-thumbnail", image, small);
                    self.thumbnails.insert(source, texture);
                }
            }
        }
    }

    /// Draws the picker. Returns false once it is closed.
    pub fn show(&mut self, app: &mut VeilApp, ui: &Ui) -> bool {
        let ctx = ui.ctx().clone();
        if self.step == Step::Desktop {
            match app.share.take_chosen() {
                Some(Ok(Some(source))) => {
                    self.selected = Some(source);
                    self.crop = None;
                    self.step = Step::SelectArea;
                    self.preview = None;
                    app.share.start_preview(source);
                }
                // Cancelled in the desktop's chooser: back to the call, nothing changed.
                Some(Ok(None)) => {
                    app.share.close_picker();
                    return false;
                }
                Some(Err(())) | None => {}
            }
        }
        self.upload(app, &ctx);
        app.share.set_windows_tab(self.tab == Tab::Application);
        let choices = app.share.choices();
        let error = app.share.error();
        let sharing = app.share.is_sharing();
        let sound = app.share.sound.support();
        let screen = ctx.content_rect().size();
        let width = (screen.x - 80.0).clamp(540.0, 900.0);
        let height = (screen.y - 230.0).clamp(240.0, 500.0);
        let mut clicked = Clicked::default();
        let modal = Modal::new(ui.scope_id().with("share-picker"))
            .frame(card())
            .show(&ctx, |ui| {
                ui.set_width(width);
                let title = match (self.step, sharing) {
                    (Step::Choose, false) => "Choose what to share",
                    (Step::Choose, true) => "Change what you share",
                    (Step::Desktop, _) => "Choose in your desktop's window",
                    (Step::SelectArea, _) => "Select an area",
                };
                ui.label(RichText::new(title).font(theme::bold(18.0)));
                ui.add_space(10.0);
                match self.step {
                    Step::Choose => {
                        self.tabs(ui);
                        ui.add_space(10.0);
                        self.grid(ui, choices.as_deref(), vec2(width, height), &mut clicked);
                    }
                    Step::Desktop => {
                        let text = "Pick a screen or window in the window your desktop opened.";
                        ui.label(RichText::new(text).color(MUTED));
                    }
                    Step::SelectArea => self.picture(ui, vec2(width, height)),
                }
                device_error(ui, error.clone());
                ui.add_space(12.0);
                self.sound_choice(ui, sound.as_ref());
                ui.add_space(8.0);
                self.footer(ui, sharing, &mut clicked);
            });
        self.apply(app, clicked, modal.should_close())
    }

    fn apply(&mut self, app: &mut VeilApp, clicked: Clicked, dismissed: bool) -> bool {
        if let Some(source) = clicked.pick
            && self.selected != Some(source)
        {
            self.selected = Some(source);
            self.crop = None;
        }
        if let (true, Some(source)) = (clicked.share, self.selected) {
            app.share
                .start(source, self.crop.map(to_crop), self.with_sound);
            return false;
        }
        if clicked.close || dismissed {
            app.share.close_picker();
            return false;
        }
        match (clicked.step, self.selected) {
            (Some(Step::SelectArea), Some(source)) => {
                self.step = Step::SelectArea;
                self.preview = None;
                app.share.start_preview(source);
            }
            (Some(Step::Desktop), _) => {
                self.step = Step::Desktop;
                self.drag_from = None;
                app.share.stop_preview();
                app.share.choose_on_desktop();
            }
            (Some(Step::Choose), _) => {
                self.step = Step::Choose;
                self.drag_from = None;
                app.share.stop_preview();
            }
            _ => {}
        }
        true
    }

    fn tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            for (tab, label) in [(Tab::Screen, "Screen"), (Tab::Application, "Application")] {
                let on = self.tab == tab;
                let colour = if on { TEXT } else { MUTED };
                let text = RichText::new(label).font(theme::bold(14.0)).color(colour);
                let response = ui.add(egui::Label::new(text).sense(Sense::click()));
                let rect = response.rect;
                if on {
                    let y = rect.bottom() + 5.0;
                    let line = Stroke::new(2.0, theme::ACCENT);
                    ui.painter().hline(rect.x_range(), y, line);
                }
                if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(5.0);
        ui.separator();
    }

    fn grid(
        &self,
        ui: &mut Ui,
        choices: Option<&[ScreenChoice]>,
        size: egui::Vec2,
        clicked: &mut Clicked,
    ) {
        let height = size.y - 34.0;
        let Some(choices) = choices else {
            ui.allocate_ui(vec2(size.x, height), |ui| {
                ui.set_min_size(vec2(size.x, height));
                ui.label(RichText::new("Looking…").color(MUTED));
            });
            return;
        };
        let shown: Vec<&ScreenChoice> =
            choices.iter().filter(|c| self.tab.holds(c.source)).collect();
        // Room is left for the scroll bar.
        let (columns, cell_w) = grid_columns(size.x - 14.0);
        let thumb_h = cell_w * 9.0 / 16.0;
        ScrollArea::vertical()
            .id_salt("share-sources")
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in shown.chunks(columns) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = CELL_GAP;
                        for choice in row {
                            self.cell(ui, choice, vec2(cell_w, thumb_h), clicked);
                        }
                    });
                    ui.add_space(CELL_GAP);
                }
            });
    }

    fn cell(&self, ui: &mut Ui, choice: &ScreenChoice, thumb: egui::Vec2, clicked: &mut Clicked) {
        let size = thumb + vec2(0.0, CELL_LABEL_H);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let label = choice.label.as_str();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        let on = self.selected == Some(choice.source);
        let lit = response.hovered() || response.has_focus();
        let picture = Rect::from_min_size(rect.min, thumb);
        let painter = ui.painter();
        let radius = CornerRadius::same(8);
        let fill = if lit { theme::RAISED_HOVER } else { theme::TILE };
        painter.rect_filled(picture, radius, fill);
        match self.thumbnails.get(&choice.source) {
            Some(texture) => {
                let image = contain(texture.size(), picture.shrink(THUMB_PAD));
                egui::Image::new((texture.id(), image.size()))
                    .corner_radius(CornerRadius::same(4))
                    .paint_at(ui, image);
            }
            // Until its first picture arrives.
            None => {
                Icon::Share.paint(painter, picture.center(), 22.0, theme::LINE);
            }
        }
        let ring = match (on, lit) {
            (true, _) => Some(Stroke::new(2.5, theme::ACCENT)),
            (false, true) => Some(Stroke::new(1.0, theme::LINE)),
            (false, false) => None,
        };
        if let Some(ring) = ring {
            painter.rect_stroke(picture, radius, ring, egui::StrokeKind::Outside);
        }
        let colour = if on || lit { TEXT } else { MUTED };
        let mut job =
            LayoutJob::simple_singleline(label.to_string(), FontId::proportional(13.0), colour);
        job.wrap = TextWrapping {
            max_width: rect.width() - 4.0,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = painter.layout_job(job);
        let at = pos2(rect.left() + 2.0, picture.bottom() + 7.0);
        painter.galley(at, galley, colour);
        let response = response.on_hover_text(label);
        if response.clicked() {
            clicked.pick = Some(choice.source);
        }
        if response.double_clicked() {
            clicked.pick = Some(choice.source);
            clicked.share = true;
        }
    }

    /// The "Include computer sound" choice, greyed out with the reason where it cannot work.
    fn sound_choice(&mut self, ui: &mut Ui, support: Option<&SoundSupport>) {
        let why = match support {
            Some(SoundSupport::Ready) => None,
            Some(SoundSupport::Unavailable(why)) => Some(*why),
            None => Some("Checking…"),
        };
        if why.is_some() {
            self.with_sound = false;
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(why.is_none(), |ui| {
                ui.checkbox(&mut self.with_sound, "Include computer sound")
                    .on_hover_text(
                        "Send what your computer plays, such as a video. Call sound is left                          out, so nobody hears an echo.",
                    )
                    .on_disabled_hover_text("Sharing sound is not available on this computer");
            });
            if let Some(why) = why {
                ui.label(RichText::new(why).small().color(MUTED));
            }
        });
    }

    fn footer(&self, ui: &mut Ui, sharing: bool, clicked: &mut Clicked) {
        ui.horizontal(|ui| {
            match self.step {
                Step::Choose => {
                    let label = if self.crop.is_some() { "Change area" } else { "Select area" };
                    ui.add_enabled_ui(self.selected.is_some(), |ui| {
                        let hint = "Share only part of this. It stays the same when you switch \
                                    to another app.";
                        if pill(ui, None, label, Tone::Quiet, 120.0).on_hover_text(hint).clicked() {
                            clicked.step = Some(Step::SelectArea);
                        }
                    });
                }
                Step::Desktop => {}
                Step::SelectArea if screen::uses_system_chooser() => {
                    if pill(ui, None, "Choose again", Tone::Quiet, 120.0).clicked() {
                        clicked.step = Some(Step::Desktop);
                    }
                    ui.label(RichText::new(self.area_hint()).small().color(MUTED));
                }
                Step::SelectArea => {
                    if pill(ui, None, "Back", Tone::Quiet, 90.0).clicked() {
                        clicked.step = Some(Step::Choose);
                    }
                    ui.label(RichText::new(self.area_hint()).small().color(MUTED));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(self.selected.is_some(), |ui| {
                    let label = if sharing { "Switch" } else { "Share" };
                    clicked.share |=
                        pill(ui, Some(Icon::Share), label, Tone::Primary, 120.0).clicked();
                });
                clicked.close = pill(ui, None, "Cancel", Tone::Quiet, 100.0).clicked();
            });
        });
    }

    fn area_hint(&self) -> &'static str {
        if self.crop.is_some() {
            "Click the picture to use all of it"
        } else {
            "Drag on the picture"
        }
    }

    /// The chosen source, with its crop drawn over it and dragged out on it.
    fn picture(&mut self, ui: &mut Ui, size: egui::Vec2) {
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

/// How many thumbnails fit across `width`, and how wide each one is.
fn grid_columns(width: f32) -> (usize, f32) {
    let columns = (((width + CELL_GAP) / (CELL_W + CELL_GAP)) as usize).max(1);
    let gaps = CELL_GAP * (columns - 1) as f32;
    (columns, (width - gaps) / columns as f32)
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

/// A shared screen, whole, with a line saying whose it is.
pub fn stage(ui: &Ui, rect: Rect, (texture, size): (TextureId, [usize; 2]), label: &str) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), Color32::BLACK);
    painter.image(texture, contain(size, rect), WHOLE, Color32::WHITE);
    let font = FontId::proportional(13.0);
    let galley = painter.layout_no_wrap(label.to_string(), font, Color32::WHITE);
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

    #[test]
    fn each_tab_holds_its_own_sources() {
        assert!(Tab::Screen.holds(ScreenSource::Monitor(1)));
        assert!(!Tab::Screen.holds(ScreenSource::Window(1)));
        assert!(Tab::Application.holds(ScreenSource::Window(1)));
        assert!(!Tab::Application.holds(ScreenSource::Monitor(1)));
    }

    #[test]
    fn thumbnails_fill_the_width_of_the_grid() {
        let (columns, width) = grid_columns(760.0);
        assert_eq!(columns, 3);
        assert!((width * 3.0 + CELL_GAP * 2.0 - 760.0).abs() < 0.01);
        assert_eq!(grid_columns(100.0), (1, 100.0));
    }
}
