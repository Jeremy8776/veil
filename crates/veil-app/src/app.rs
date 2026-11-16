//! The app's screens and what they share: your name, your devices and the camera texture.
//!
//! Purpose: [`VeilApp`] owns the state that outlives one screen and switches between Home,
//! Pre-join, the call, and the end-of-call screen. Each screen lives in `screens/`.
//!
//! Invariants:
//! - Devices are on only on the pre-join and call screens. Leaving either turns them off.
//! - The camera texture is updated once per frame, before any screen draws.

use std::time::{Duration, Instant};

use eframe::egui::{self, TextureHandle, TextureOptions};

use crate::media::LocalMedia;
use crate::model::Call;
use crate::screens::call::CallView;

pub enum Screen {
    Home,
    PreJoin { host: bool },
    Call(Box<CallView>),
    Ended { title: String, lasted: Duration },
}

pub struct VeilApp {
    pub screen: Screen,
    pub name: String,
    pub invite: String,
    pub home_error: Option<String>,
    pub media: LocalMedia,
    /// Your camera, as last uploaded.
    pub preview: Option<TextureHandle>,
    /// Microphone level, smoothed for the meter.
    pub level: f32,
    /// What you chose on the pre-join screen, kept for the call.
    pub want_camera: bool,
    pub want_mic: bool,
    /// Devices on this machine, read when the pre-join screen opens.
    pub devices: crate::devices::DeviceLists,
}

impl VeilApp {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            screen: Screen::Home,
            name: std::env::var("USERNAME")
                .ok()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_default(),
            invite: String::new(),
            home_error: None,
            media: LocalMedia::new(ctx.clone()),
            preview: None,
            level: 0.0,
            want_camera: true,
            want_mic: true,
            devices: crate::devices::DeviceLists::default(),
        }
    }

    /// Opens the pre-join screen with your devices on, as you last chose.
    pub fn go_pre_join(&mut self, host: bool) {
        self.devices = crate::devices::DeviceLists::load();
        self.media.set_camera(self.want_camera);
        self.media.set_mic(self.want_mic);
        self.screen = Screen::PreJoin { host };
    }

    pub fn go_call(&mut self, host: bool) {
        let mut call = crate::demo::call(self.name.trim(), host, Instant::now());
        if let Some(you) = call.you_mut() {
            you.muted = !self.want_mic;
            you.camera = self.want_camera;
        }
        self.screen = Screen::Call(Box::new(CallView::new(call)));
    }

    pub fn leave(&mut self, call: &Call) {
        self.media.set_camera(false);
        self.media.set_mic(false);
        self.screen = Screen::Ended {
            title: call.title.clone(),
            lasted: call.elapsed(Instant::now()),
        };
    }

    pub fn go_home(&mut self) {
        self.media.set_camera(false);
        self.media.set_mic(false);
        self.preview = None;
        self.screen = Screen::Home;
    }

    /// Uploads the newest camera picture and smooths the microphone level.
    fn update_media(&mut self, ctx: &egui::Context) {
        if !self.media.camera_on() {
            self.preview = None;
        } else if let Some(picture) = self.media.take_picture() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [picture.width, picture.height],
                &picture.pixels,
            );
            match &mut self.preview {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.preview = Some(ctx.load_texture("camera", image, TextureOptions::LINEAR));
                }
            }
        }
        let target = self.media.level();
        // Fast attack, slow release, like a hardware meter.
        let rate = if target > self.level { 0.6 } else { 0.15 };
        self.level += (target - self.level) * rate;
    }

    /// Your camera texture and its size, for a tile.
    pub fn preview_video(&self) -> Option<(egui::TextureId, [usize; 2], bool)> {
        self.preview.as_ref().map(|t| (t.id(), t.size(), true))
    }
}

impl eframe::App for VeilApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update_media(&ctx);
        match std::mem::replace(&mut self.screen, Screen::Home) {
            Screen::Home => {
                self.screen = Screen::Home;
                crate::screens::home::show(self, ui);
            }
            Screen::PreJoin { host } => {
                self.screen = Screen::PreJoin { host };
                crate::screens::prejoin::show(self, ui, host);
            }
            Screen::Call(mut view) => {
                if view.show(self, ui) {
                    self.screen = Screen::Call(view);
                } else {
                    self.leave(&view.call);
                }
            }
            Screen::Ended { title, lasted } => {
                self.screen = Screen::Ended { title, lasted };
                crate::screens::home::ended(self, ui);
            }
        }
    }
}
