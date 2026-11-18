//! The app's screens and what they share: your name, your devices and the camera texture.
//!
//! Purpose: [`VeilApp`] owns the state that outlives one screen and switches between Home,
//! Pre-join, the call, the end-of-call screen and Settings > Connection. Each screen lives in
//! `screens/`.
//!
//! Invariants:
//! - Devices are on only on the pre-join and call screens. Leaving either turns them off.
//! - The camera texture is updated once per frame, before any screen draws.
//! - A real call's engine lives in its [`CallView`]; your camera and microphone feed it only
//!   while that view exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, TextureHandle, TextureOptions};

use crate::engine::{Engine, Role};
use crate::keystore::{self, KeyStore};
use crate::media::LocalMedia;
use crate::model::{Call, CallStatus};
use crate::screens::call::CallView;

pub enum Screen {
    Home,
    PreJoin {
        host: bool,
    },
    Call(Box<CallView>),
    /// Settings > Connection, with the fields being edited.
    Connection(Box<crate::screens::connection::Form>),
    Ended {
        title: String,
        lasted: Duration,
        reason: String,
    },
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
    /// The invite link being joined, checked on the home screen.
    pub joining: Option<veil_security::short_invite::InviteLink>,
    /// The next call is the simulated demo.
    pub demo: bool,
    /// Everyone else's video in the current call, by participant id.
    pub remote: HashMap<u32, TextureHandle>,
    /// Whether Windows lets guests reach you, checked before you host.
    pub firewall: crate::firewall::Firewall,
    /// Your device key and the keys you verified, kept between calls.
    pub keys: Arc<Mutex<KeyStore>>,
    /// "New device key" was clicked once and waits for a second click.
    pub confirm_new_key: bool,
    /// Whether a newer Veil is out (`update.rs`).
    pub updates: crate::update::Updates,
    /// Your own servers and port, kept between runs (`connection.rs`).
    pub connection: crate::connection::Store,
    ctx: egui::Context,
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
            joining: None,
            demo: false,
            remote: HashMap::new(),
            firewall: crate::firewall::Firewall::new(),
            keys: Arc::new(Mutex::new(KeyStore::open(keystore::default_dir()))),
            confirm_new_key: false,
            updates: crate::update::Updates::start(keystore::default_dir(), ctx),
            connection: crate::connection::Store::open(keystore::default_dir()),
            ctx: ctx.clone(),
        }
    }

    /// Opens the pre-join screen with your devices on, as you last chose.
    pub fn go_pre_join(&mut self, host: bool) {
        self.devices = crate::devices::DeviceLists::load();
        self.media.set_camera(self.want_camera);
        self.media.set_mic(self.want_mic);
        if host && !self.demo {
            self.firewall.check(&self.ctx);
        }
        self.screen = Screen::PreJoin { host };
    }

    /// Starts the call: the demo, or the real engine as host or guest.
    pub fn go_call(&mut self, host: bool) {
        let now = Instant::now();
        let name = self.name.trim().to_string();
        let (mut call, engine) = if self.demo {
            (crate::demo::call(&name, host, now), None)
        } else {
            let role = match (host, self.joining.clone()) {
                (true, _) => Role::Host,
                (false, Some(invite)) => Role::Guest(Box::new(invite)),
                (false, None) => {
                    self.home_error = Some(String::from("Paste the invite link you were sent."));
                    self.go_home();
                    return;
                }
            };
            let playback = self.media.playback();
            let keys = self.keys.clone();
            let config = self.connection.config();
            match Engine::start(role, config, name.clone(), playback, keys, self.ctx.clone()) {
                Ok(engine) => {
                    self.media.set_taps(Some(engine.taps()));
                    let title = if host {
                        format!("{name}'s call")
                    } else {
                        String::from("Veil call")
                    };
                    (Call::live(title, &name, host, now), Some(engine))
                }
                Err(e) => {
                    self.home_error = Some(e);
                    self.go_home();
                    return;
                }
            }
        };
        if let Some(you) = call.you_mut() {
            you.muted = !self.want_mic;
            you.camera = self.want_camera;
        }
        self.remote.clear();
        self.screen = Screen::Call(Box::new(CallView::new(call, engine)));
    }

    pub fn leave(&mut self, call: &Call) {
        self.media.set_taps(None);
        self.media.set_camera(false);
        self.media.set_mic(false);
        self.remote.clear();
        let reason = match &call.status {
            CallStatus::Ended(reason) => reason.clone(),
            _ => String::from("You left the call"),
        };
        self.screen = Screen::Ended {
            title: call.title.clone(),
            lasted: call.elapsed(Instant::now()),
            reason,
        };
    }

    /// Uploads pictures that arrived for other people's tiles.
    pub fn upload_remote(&mut self, pictures: Vec<(u32, crate::convert::Rgba)>) {
        for (id, picture) in pictures {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [picture.width, picture.height],
                &picture.pixels,
            );
            match self.remote.get_mut(&id) {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    let texture = self.ctx.load_texture(
                        format!("person-{id}"),
                        image,
                        TextureOptions::LINEAR,
                    );
                    self.remote.insert(id, texture);
                }
            }
        }
    }

    /// Someone else's newest picture, for their tile.
    pub fn remote_video(&self, id: u32) -> Option<(egui::TextureId, [usize; 2])> {
        self.remote.get(&id).map(|t| (t.id(), t.size()))
    }

    pub fn go_home(&mut self) {
        self.media.set_taps(None);
        self.media.set_camera(false);
        self.media.set_mic(false);
        self.preview = None;
        self.screen = Screen::Home;
    }

    /// Uploads the newest camera picture and smooths the microphone level.
    fn update_media(&mut self, ctx: &egui::Context) {
        if self.media.take_camera_stopped() {
            self.want_camera = false;
        }
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

    /// Your camera texture and its size, for a tile. It shows exactly what you send.
    pub fn preview_video(&self) -> Option<(egui::TextureId, [usize; 2])> {
        self.preview.as_ref().map(|t| (t.id(), t.size()))
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
            Screen::Connection(mut form) => {
                if crate::screens::connection::show(self, ui, &mut form) {
                    self.screen = Screen::Connection(form);
                }
            }
            Screen::Ended {
                title,
                lasted,
                reason,
            } => {
                self.screen = Screen::Ended {
                    title,
                    lasted,
                    reason,
                };
                crate::screens::home::ended(self, ui);
            }
        }
    }
}
