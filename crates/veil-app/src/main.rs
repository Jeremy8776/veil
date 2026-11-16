//! Veil's desktop app: start or join a call, check your camera and microphone, and talk.
//!
//! Purpose: the window and start-up. The screens are in `screens/`, the look in `theme.rs`,
//! and the call state in `model.rs`. Until the app is wired to `veil-call`, the other people
//! in a call are simulated (`demo.rs`) and the interface says so; your own camera and
//! microphone are real and never leave the device.
//!
//! Invariants:
//! - No `unsafe`: the workspace forbids it, and the native media code lives in `veil-media`.
//! - No bundled fonts or icons: both come from the operating system (`theme.rs`).

// Release builds open no console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod convert;
mod demo;
mod devices;
mod icons;
mod layout;
mod media;
mod model;
mod screens;
mod theme;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Veil")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([880.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Veil",
        options,
        Box::new(|cc| {
            let found = theme::load_fonts(&cc.egui_ctx)?;
            icons::set_available(found.icons);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::VeilApp::new(&cc.egui_ctx)))
        }),
    )
}
