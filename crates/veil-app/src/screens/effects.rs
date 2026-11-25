//! The effects controls shared by the call bar's Effects menu, the pre-join screen and the
//! phone layout: the colour look (built-in Filters and imported LUTs), importing and removing
//! a LUT, and a Strength slider for the look and for Background blur.
//!
//! Purpose: one place decides how a LUT is imported ([`start_import`], [`take_imports`]) and
//! how strength is shown, so every screen behaves the same. See ADR 0008.
//!
//! Invariants:
//! - Importing never blocks the window: the system chooser runs on its own thread (or, on
//!   macOS, as a panel queued on the main thread) and [`take_imports`] collects the result.
//! - A refused file leaves the current look as it was and shows the reason in the menu.
//! - Nothing here opens a pop-up inside the call menu (see `call_menus.rs`).

use eframe::egui::{self, Align2, FontId, Sense, Ui, vec2};
use veil_media::effects::FULL;
use veil_media::lut::MAX_FILE;
use veil_media::pick::{self, Outcome};
use veil_media::{Blur, Filter};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::luts::Library;
use crate::media::LocalMedia;
use crate::screens::call_menus;
use crate::theme::{self, MUTED};

/// Tooltip for the look's strength.
pub const LOOK_STRENGTH_HINT: &str =
    "How strongly the filter or LUT is applied. 0% is your normal picture.";
/// Tooltip for blur's strength.
pub const BLUR_STRENGTH_HINT: &str =
    "How much of the blurred background replaces the real one. 0% is no blur.";
const IMPORT_HINT: &str =
    "Add a colour look from a .cube LUT file. Veil keeps its own copy on this device.";

/// Opens the system's file chooser for a `.cube` file. Does nothing if one is open.
pub fn start_import(luts: &Library) {
    luts.clear_notice();
    pick::start("Import a LUT", "LUT (.cube)", "*.cube", MAX_FILE);
}

/// Called once a frame: imports a file the chooser returned or one dropped on the window,
/// and uses it as the look.
pub fn take_imports(app: &VeilApp, ctx: &egui::Context) {
    let dropped = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .find(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("cube"))
            })
    });
    let outcome = match dropped {
        Some(path) => Some(match pick::read_capped(&path, MAX_FILE) {
            Ok(picked) => Outcome::Picked(picked),
            Err(why) => Outcome::Failed(why),
        }),
        None => pick::poll(),
    };
    match outcome {
        Some(Outcome::Picked(picked)) => {
            if let Ok(look) = app.luts.import(&picked) {
                app.media.set_lut(Some(look));
            }
            ctx.request_repaint();
        }
        Some(Outcome::Failed(why)) => {
            app.luts.set_notice(why);
            ctx.request_repaint();
        }
        Some(Outcome::Cancelled) => {}
        // Android reports back by polling, so look again shortly while a chooser is open.
        None if cfg!(target_os = "android") => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        None => {}
    }
}

/// Chooses the LUT called `name`, or shows why it no longer loads.
pub fn choose_lut(app: &VeilApp, name: &str) {
    if let Some(look) = app.luts.load(name) {
        app.media.set_lut(Some(look));
    }
}

/// The name of the look in use, for a drop-down's closed state.
pub fn look_label(media: &LocalMedia) -> String {
    match media.lut() {
        Some(look) => look.name,
        None => filter_label(media.filter()).to_string(),
    }
}

pub fn filter_label(filter: Filter) -> &'static str {
    match filter {
        Filter::None => "None",
        Filter::Warm => "Warm",
        Filter::Cool => "Cool",
        Filter::BlackWhite => "Black & white",
        Filter::Bright => "Bright",
    }
}

pub fn blur_label(blur: Blur) -> &'static str {
    match blur {
        Blur::Off => "Off",
        Blur::Light => "Light",
        Blur::Strong => "Strong",
    }
}

/// A Strength row: the word, a 0 to 100% slider and the value. Returns the new percent when
/// it moved.
pub fn strength(ui: &mut Ui, id: &str, percent: u8, hint: &str) -> Option<u8> {
    let mut changed = None;
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (label, _) = ui.allocate_exact_size(vec2(58.0, 24.0), Sense::hover());
            ui.painter().text(
                label.left_center(),
                Align2::LEFT_CENTER,
                "Strength",
                FontId::proportional(12.5),
                MUTED,
            );
            let mut value = f32::from(percent) / 100.0;
            let width = (ui.available_width() - 44.0).max(40.0);
            let moved = call_menus::slider(ui, &mut value, 1.0, width, "Strength")
                .on_hover_text(hint)
                .changed();
            if moved {
                changed = Some((value * 100.0).round().clamp(0.0, f32::from(FULL)) as u8);
            }
            let (text, _) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::hover());
            ui.painter().text(
                text.right_center(),
                Align2::RIGHT_CENTER,
                format!("{}%", changed.unwrap_or(percent)),
                FontId::proportional(12.5),
                MUTED,
            );
        });
    });
    changed
}

/// The Effects menu's look section: Filter swatches, imported LUTs to tick or remove, Import
/// and the look's strength.
pub fn look_menu(app: &VeilApp, ui: &mut Ui) {
    call_menus::filter_swatches(ui, &app.media);
    let chosen = app.media.lut().map(|look| look.name);
    let mut remove = None;
    for name in app.luts.names() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let this = chosen.as_deref() == Some(name.as_str());
            let width = ui.available_width() - 30.0;
            let row = ui
                .allocate_ui(vec2(width, 32.0), |ui| call_menus::choice(ui, &name, this))
                .inner;
            if row.on_hover_text("Imported LUT").clicked() && !this {
                choose_lut(app, &name);
            }
            let close = icon_button(ui, Icon::Close, &format!("Remove {name}"));
            if close.clicked() {
                remove = Some(name.clone());
            }
        });
    }
    if let Some(name) = remove {
        if chosen.as_deref() == Some(name.as_str()) {
            app.media.set_lut(None);
        }
        app.luts.remove(&name);
    }
    let import = call_menus::toggle_button(ui, Icon::Folder, "Import LUT…", false);
    if import.on_hover_text(IMPORT_HINT).clicked() {
        start_import(&app.luts);
    }
    notice(ui, &app.luts);
    if let Some(p) = strength(
        ui,
        "look strength",
        app.media.look_strength(),
        LOOK_STRENGTH_HINT,
    ) {
        app.media.set_look_strength(p);
    }
}

/// The Effects menu's blur section: Off, Light and Strong, then its strength.
pub fn blur_menu(app: &VeilApp, ui: &mut Ui) {
    call_menus::blur_switch(ui, &app.media);
    if app.media.blur() != Blur::Off
        && let Some(p) = strength(
            ui,
            "blur strength",
            app.media.blur_strength(),
            BLUR_STRENGTH_HINT,
        )
    {
        app.media.set_blur_strength(p);
    }
}

/// The pre-join look drop-down: Filters, imported LUTs and Import, then the strength.
pub fn look_picker(app: &VeilApp, ui: &mut Ui) {
    crate::widgets::picker_label(ui, Icon::Effects, "Filter");
    let lut = app.media.lut().map(|look| look.name);
    let filter = app.media.filter();
    egui::ComboBox::from_id_salt("Filter")
        .width(280.0)
        .selected_text(look_label(&app.media))
        .show_ui(ui, |ui| {
            for f in Filter::ALL {
                let on = lut.is_none() && f == filter;
                if ui.selectable_label(on, filter_label(f)).clicked() && !on {
                    app.media.set_filter(f);
                }
            }
            let names = app.luts.names();
            if !names.is_empty() {
                ui.separator();
            }
            for name in names {
                let on = lut.as_deref() == Some(name.as_str());
                if ui.selectable_label(on, &name).clicked() && !on {
                    choose_lut(app, &name);
                }
            }
            ui.separator();
            if ui
                .selectable_label(false, "Import LUT…")
                .on_hover_text(IMPORT_HINT)
                .clicked()
            {
                start_import(&app.luts);
            }
        });
    notice(ui, &app.luts);
    if let Some(p) = strength(
        ui,
        "look strength",
        app.media.look_strength(),
        LOOK_STRENGTH_HINT,
    ) {
        app.media.set_look_strength(p);
    }
}

/// The pre-join Background blur drop-down, with its strength and the notice if blur turned
/// itself off.
pub fn blur_picker(app: &VeilApp, ui: &mut Ui) {
    let media = &app.media;
    crate::widgets::picker_label(ui, Icon::Effects, "Background blur");
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
    if current != Blur::Off
        && let Some(p) = strength(
            ui,
            "blur strength",
            media.blur_strength(),
            BLUR_STRENGTH_HINT,
        )
    {
        media.set_blur_strength(p);
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

/// Why the last import or removal failed, in small red text, until the next try.
pub fn notice(ui: &mut Ui, luts: &Library) {
    if let Some(why) = luts.notice() {
        ui.label(egui::RichText::new(why).small().color(theme::DANGER_HOVER));
    }
}

/// A small square button with only an icon; its name is the tooltip and the screen-reader
/// label.
fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(28.0, 32.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let colour = if response.hovered() || response.has_focus() {
        theme::TEXT
    } else {
        MUTED
    };
    icon.paint(ui.painter(), rect.center(), 13.0, colour);
    response.on_hover_text(label)
}
