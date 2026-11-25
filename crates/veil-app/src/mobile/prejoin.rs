//! The phone Start a call and Join a call screen.
//!
//! The camera preview takes all the space the controls leave, so the screen never scrolls.
//! Below it sit four round controls (mute, video, mirror, data use) and the main action.
//! Nothing explains itself on screen: detail lives in tooltips. Spacing keeps an 8/16 rhythm.
use super::{EDGE, action, app_bar, data_menu, flip, widgets as w};
use crate::app::VeilApp;
use crate::data_use::DataUse;
use crate::icons::Icon;
use crate::screens::controls;
use crate::theme;
use crate::widgets::{self as shared, Tile, Tone};
use eframe::egui::{self, Color32, CornerRadius, Margin, Rect, Sense, UiBuilder, pos2, vec2};
use std::time::Duration;

fn margin(top: i8, bottom: i8) -> Margin {
    Margin {
        left: EDGE,
        right: EDGE,
        top,
        bottom,
    }
}

pub fn prejoin(app: &mut VeilApp, ui: &mut egui::Ui, host: bool) {
    let (back, _) = app_bar(
        ui,
        "mobile-prejoin-bar",
        if host { "Start a call" } else { "Join a call" },
        true,
        false,
    );
    if back {
        app.go_home();
        return;
    }
    #[cfg(target_os = "android")]
    if veil_media::ffi::android::resumed() {
        let (camera, mic) = veil_media::ffi::android::permission_state();
        app.media.set_camera(app.want_camera && camera);
        app.media.set_mic(app.want_mic && mic);
    }
    egui::Panel::bottom("mobile-join-action")
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(margin(8, 16)))
        .show_separator_line(false)
        .show(ui, |ui| {
            if let Some(why) = app.connection.config().conflict() {
                w::label(ui, why, 13.0, false, theme::WARN);
            } else if action(
                ui,
                Some(Icon::Video),
                if host { "Start call" } else { "Join now" },
                Tone::Primary,
            ) {
                app.go_call(host);
            }
        });
    // The controls sit in their own panel above the action, so the preview takes only the
    // space left over.
    egui::Panel::bottom("mobile-prejoin-controls")
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(margin(16, 8)))
        .show_separator_line(false)
        .show(ui, |ui| controls_row(app, ui));
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(margin(8, 0)))
        .show(ui, |ui| {
            let tile = Tile {
                name: &app.name,
                video: app.preview_video(),
                muted: !app.media.mic_on(),
                speaking: false,
                hand_raised: false,
                is_you: true,
            };
            let rect = preview_card(ui, &tile);
            let mut overlay = ui.new_child(UiBuilder::new().id_salt("mobile-camera-flip").max_rect(
                Rect::from_min_size(rect.right_top() + vec2(-56.0, 8.0), vec2(48.0, 48.0)),
            ));
            if w::icon(&mut overlay, Icon::Refresh, "Switch camera", true).clicked() {
                flip(app);
            }
        });
    ui.ctx().request_repaint_after(Duration::from_millis(100));
}

/// Mute (ringed by the microphone level), video, mirror and data use, then any errors.
fn controls_row(app: &mut VeilApp, ui: &mut egui::Ui) {
    ui.columns(4, |columns| {
        let mic = app.media.mic_on();
        let mute = w::call_control(
            &mut columns[0],
            Icon::Mic,
            if mic { "Mute" } else { "Unmute" },
            !mic,
            false,
        )
        .on_hover_text("The ring shows your microphone level");
        if mic {
            w::level_ring(&columns[0], &mute, app.level);
        }
        if mute.clicked() {
            controls::toggle_mic(app);
        }
        let camera = app.media.camera_on();
        if w::call_control(
            &mut columns[1],
            Icon::Video,
            if camera { "Stop video" } else { "Start video" },
            !camera,
            false,
        )
        .clicked()
        {
            controls::toggle_camera(app);
        }
        let mirrored = app.media.mirror();
        if w::call_control(
            &mut columns[2],
            Icon::Mirror,
            if mirrored { "Mirrored" } else { "Mirror" },
            false,
            false,
        )
        .on_hover_text(controls::MIRROR_HINT)
        .clicked()
        {
            app.media.set_mirror(!mirrored);
        }
        let data = w::call_control(
            &mut columns[3],
            Icon::Signal,
            data_label(app.data_use.get()),
            false,
            false,
        )
        .on_hover_text("Data use");
        data_menu(app, &data);
    });
    if let Some(error) = app.media.camera_error() {
        ui.add_space(8.0);
        w::label(ui, error, 13.0, false, theme::WARN);
    }
    if let Some(error) = app.media.mic_error() {
        ui.add_space(8.0);
        w::label(ui, error, 13.0, false, theme::WARN);
    }
    #[cfg(target_os = "android")]
    if !veil_media::ffi::android::permissions_ready() {
        ui.add_space(8.0);
        if action(ui, None, "Allow camera and microphone", Tone::Quiet) {
            veil_media::ffi::android::request_permissions();
        }
    }
}

/// The short label under the data use control.
fn data_label(choice: DataUse) -> &'static str {
    match choice {
        DataUse::Auto => "Auto data",
        DataUse::Saver => "Data saver",
        DataUse::AudioOnly => "Audio only",
    }
}

/// The camera preview: the video fills the card, with an equal mild border on every side
/// and the name over the picture. Returns the video's rect, for controls laid over it.
fn preview_card(ui: &mut egui::Ui, tile: &Tile<'_>) -> Rect {
    const BORDER: f32 = 6.0;
    let available = ui.available_rect_before_wrap();
    let Some((texture, size)) = tile.video else {
        let (rect, _) = ui.allocate_exact_size(available.size(), Sense::hover());
        shared::tile(ui, rect, tile);
        return rect.shrink(BORDER);
    };
    let picture = crate::layout::contain(size, available.shrink(BORDER));
    let card = picture.expand(BORDER);
    ui.allocate_rect(available, Sense::hover());
    let painter = ui.painter_at(card);
    painter.rect_filled(card, CornerRadius::same(theme::RADIUS + BORDER as u8), theme::TILE);
    painter.add(
        egui::epaint::RectShape::filled(picture, CornerRadius::same(theme::RADIUS), Color32::WHITE)
            .with_texture(texture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
    );
    shared::name_tag(&painter, picture, tile);
    picture
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_labels_stay_short() {
        for choice in DataUse::ALL {
            assert!(data_label(choice).len() <= 10, "{choice:?}");
        }
    }
}
