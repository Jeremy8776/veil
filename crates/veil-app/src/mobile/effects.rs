//! The phone layout's effects: Filter or imported LUT, Import, Background blur and the
//! strength of each, on the pre-join page.
//!
//! Purpose: the same choices as the desktop Effects menu (`screens/effects.rs`), drawn with
//! the phone's large touch targets. Choices made here stay on for the call.
//!
//! Invariants:
//! - Each drop-down is a single pop-up on the page, never one inside another.
//! - Removing a LUT is offered only for the one in use, so a tap in a long list cannot
//!   remove the wrong one.

use eframe::egui::{self, Align, Layout, vec2};
use veil_media::Blur;
use veil_media::Filter;

use super::widgets as w;
use crate::app::VeilApp;
use crate::screens::effects as fx;
use crate::theme;
use crate::widgets::Tone;

/// A titled row with a button that opens a menu of `draw`.
fn menu_row(ui: &mut egui::Ui, title: &str, value: &str, draw: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 188.0).max(1.0);
        ui.allocate_ui_with_layout(
            vec2(width, 48.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(width);
                w::label(ui, title, 16.0, false, theme::TEXT);
            },
        );
        let response = w::action(ui, None, value, Tone::Quiet, 180.0);
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(260.0);
            draw(ui);
        });
    });
}

fn item(ui: &mut egui::Ui, label: &str, chosen: bool) -> bool {
    let clicked = ui
        .add_sized(
            [ui.available_width(), 48.0],
            egui::Button::new(label).selected(chosen),
        )
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Filter or LUT, its strength, Background blur and its strength.
pub fn section(app: &VeilApp, ui: &mut egui::Ui) {
    let lut = app.media.lut().map(|look| look.name);
    let filter = app.media.filter();
    menu_row(ui, "Filter", &fx::look_label(&app.media), |ui| {
        for f in Filter::ALL {
            let on = lut.is_none() && f == filter;
            if item(ui, fx::filter_label(f), on) && !on {
                app.media.set_filter(f);
            }
        }
        for name in app.luts.names() {
            let on = lut.as_deref() == Some(name.as_str());
            if item(ui, &name, on) && !on {
                fx::choose_lut(app, &name);
            }
        }
        if item(ui, "Import LUT…", false) {
            fx::start_import(&app.luts);
        }
        if let Some(name) = &lut
            && item(ui, &format!("Remove {name}"), false)
        {
            app.media.set_lut(None);
            app.luts.remove(name);
        }
    });
    fx::notice(ui, &app.luts);
    if let Some(p) = fx::strength(
        ui,
        "look strength",
        app.media.look_strength(),
        fx::LOOK_STRENGTH_HINT,
    ) {
        app.media.set_look_strength(p);
    }
    let blur = app.media.blur();
    menu_row(ui, "Background blur", fx::blur_label(blur), |ui| {
        for b in Blur::ALL {
            if item(ui, fx::blur_label(b), b == blur) && b != blur {
                app.media.set_blur(b);
            }
        }
    });
    if blur != Blur::Off
        && let Some(p) = fx::strength(
            ui,
            "blur strength",
            app.media.blur_strength(),
            fx::BLUR_STRENGTH_HINT,
        )
    {
        app.media.set_blur_strength(p);
    }
    if let Some(notice) = app.media.blur_notice() {
        ui.label(
            egui::RichText::new(notice)
                .small()
                .color(theme::DANGER_HOVER),
        );
    }
}
