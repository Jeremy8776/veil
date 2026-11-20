//! Veil's desktop app: start or join a call, check your camera and microphone, and talk.
//!
//! Purpose: the window and start-up. The screens are in `screens/`, the look in `theme.rs`,
//! the call state the screens read in `model.rs`, and the real call (network, keys, encoding,
//! decoding, playback) in `engine/`. `demo.rs` simulates a call behind the "Demo call" entry
//! on Home, and the interface says so.
//!
//! Invariants:
//! - No `unsafe`: the workspace forbids it, and the native media code lives in `veil-media`.
//! - No bundled fonts or UI icons: both come from the operating system (`theme.rs`). The one
//!   bundled image is Veil's own app icon (`assets/`), set here and embedded by `build.rs`.

// Release builds open no console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chat;
mod connection;
mod convert;
mod data_use;
mod demo;
#[cfg(feature = "dev-ui")]
mod dev_start;
mod devices;
mod engine;
mod firewall;
mod icons;
mod json_file;
mod keystore;
mod layout;
mod lobby_alert;
mod log;
mod logo;
mod media;
mod model;
mod preferences;
mod screens;
mod share;
mod system_fonts;
mod theme;
mod update;
mod volume;
mod widgets;

/// The main window's title. Screen sharing also finds the window by it.
const WINDOW_TITLE: &str = "Veil";

use eframe::egui;

/// The 256 px frame of `assets/veil.ico`, for the title bar, taskbar button and Alt+Tab.
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/veil-256.png");

fn main() -> eframe::Result {
    // A copy started only to wait on Windows Installer and reopen Veil (`update/apply.rs`).
    // Before the log starts, so the last real run's log is kept.
    if update::finish_from_args() {
        return Ok(());
    }
    log::start(log::folder().as_deref());
    let mut viewport = egui::ViewportBuilder::default().with_title(WINDOW_TITLE);
    // A bad icon is cosmetic: the window opens with the default icon instead.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            // Matches the Linux .desktop file (packaging/linux), so docks show Veil's icon.
            .with_app_id("io.github.jeremy8776.veil")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([880.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    log::record_exit(eframe::run_native(
        "Veil",
        options,
        Box::new(|cc| {
            let found = theme::load_fonts(&cc.egui_ctx)?;
            icons::set_segoe(found.segoe_icons);
            theme::apply(&cc.egui_ctx);
            let app = app::VeilApp::new(&cc.egui_ctx);
            #[cfg(feature = "dev-ui")]
            let app = dev_start::opened(app);
            Ok(Box::new(app))
        }),
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    #[test]
    fn app_icon_decodes_at_256_px() {
        let icon = eframe::icon_data::from_png_bytes(super::APP_ICON_PNG).expect("valid png");
        assert_eq!((icon.width, icon.height), (256, 256));
    }
}
