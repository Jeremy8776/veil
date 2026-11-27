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
    pub log_opener: crate::log::FolderOpener,
    pub diagnostics: crate::diagnostics::review::Review,
    pub media: LocalMedia,
    /// Imported LUTs (`luts.rs`), listed with the Filters.
    pub luts: crate::luts::Library,
    /// Sharing your screen (`share.rs`).
    pub share: crate::share::ScreenShare,
    /// What you are sending while you share, as last uploaded.
    share_view: Option<TextureHandle>,
    /// Your camera, as last uploaded.
    pub preview: Option<TextureHandle>,
    /// Microphone level, smoothed for the meter.
    pub level: f32,
    /// When the microphone level last passed the speaking threshold, for your border.
    pub loud_at: Option<Instant>,
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
    /// Where each person's head is, for tiles that crop their picture.
    pub framing: crate::framing::Framing,
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
    pub nearby: crate::nearby::Nearby,
    /// How much data calls use, shared with the running call (`data_use.rs`).
    pub data_use: Arc<crate::data_use::DataUseCell>,
    /// Your devices, filter, blur, mirror and volumes, kept between runs (`preferences.rs`).
    prefs: crate::preferences::Store,
    /// Notifies and flashes the taskbar when someone knocks while you look elsewhere.
    lobby_alert: crate::lobby_alert::LobbyAlert,
    /// The host's Lobby alerts switch, saved in `preferences.rs`. On by default.
    pub lobby_alerts: bool,
    ctx: egui::Context,
}

impl VeilApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let connection = crate::connection::Store::open(keystore::default_dir());
        let updates = crate::update::Updates::start_with_network(
            keystore::default_dir(),
            ctx,
            !connection.saved.local_only && !cfg!(target_os = "android"),
        );
        let data_use = Arc::new(crate::data_use::DataUseCell::new(connection.saved.data_use));
        let prefs = crate::preferences::Store::open(keystore::default_dir());
        let lobby_alerts = prefs.saved.lobby_alerts.unwrap_or(true);
        let mut framing = crate::framing::Framing::new(ctx.clone());
        framing.set_enabled(prefs.saved.face_framing.unwrap_or(true));
        let media = LocalMedia::new(ctx.clone());
        let luts = crate::luts::Library::open(keystore::default_dir());
        prefs.saved.apply(&media, &luts);
        #[cfg(target_os = "android")]
        let name = prefs.saved.display_name.clone();
        #[cfg(not(target_os = "android"))]
        // Windows sets USERNAME; macOS and Linux set USER.
        let name = ["USERNAME", "USER"]
            .iter()
            .filter_map(|var| std::env::var(var).ok())
            .find(|n| !n.trim().is_empty())
            .unwrap_or_default();
        Self {
            screen: Screen::Home,
            name,
            invite: String::new(),
            home_error: None,
            log_opener: crate::log::FolderOpener::default(),
            diagnostics: crate::diagnostics::review::Review::default(),
            media,
            luts,
            share: crate::share::ScreenShare::new(ctx.clone()),
            share_view: None,
            preview: None,
            level: 0.0,
            loud_at: None,
            want_camera: true,
            want_mic: true,
            devices: crate::devices::DeviceLists::default(),
            joining: None,
            demo: false,
            remote: HashMap::new(),
            framing,
            firewall: crate::firewall::Firewall::new(),
            keys: Arc::new(Mutex::new(KeyStore::open(keystore::key_dir()))),
            confirm_new_key: false,
            updates,
            connection,
            nearby: crate::nearby::Nearby::default(),
            data_use,
            prefs,
            lobby_alert: crate::lobby_alert::LobbyAlert::default(),
            lobby_alerts,
            ctx: ctx.clone(),
        }
    }

    /// Opens the pre-join screen with your devices on, as you last chose.
    pub fn go_pre_join(&mut self, host: bool) {
        #[cfg(target_os = "android")]
        veil_media::ffi::android::request_permissions();
        self.devices = crate::devices::DeviceLists::load();
        // Before the devices open, so the saved ones open first time.
        let saved = self.prefs.saved.devices(&self.devices);
        self.media.choose_camera(saved.camera);
        self.media.choose_mic(saved.mic);
        self.media.choose_speaker(saved.speaker);
        let (camera_ready, mic_ready) = {
            #[cfg(target_os = "android")]
            {
                veil_media::ffi::android::permission_state()
            }
            #[cfg(not(target_os = "android"))]
            {
                (true, true)
            }
        };
        self.media.set_camera(self.want_camera && camera_ready);
        self.media.set_mic(self.want_mic && mic_ready);
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
            let data = self.data_use.clone();
            data.set_auto_saving(false);
            match Engine::start(
                role,
                config,
                name.clone(),
                playback,
                keys,
                data,
                self.ctx.clone(),
            ) {
                Ok(engine) => {
                    self.media.set_taps(Some(engine.taps()));
                    self.share.set_tap(Some(engine.taps().screen));
                    self.share.sound.set_tap(Some(engine.taps().share_sound));
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
        #[cfg(target_os = "android")]
        veil_media::ffi::android::call_active(true);
    }

    /// Uses `choice` from now on, in the running call too, and saves it. Audio only also
    /// turns your camera off, as Stop video does, so your tile shows that.
    pub fn set_data_use(&mut self, choice: crate::data_use::DataUse) {
        self.data_use.set(choice);
        self.connection.set_data_use(choice);
        if !choice.sends_camera() && self.media.camera_on() {
            self.want_camera = false;
            self.media.set_camera(false);
        }
    }

    pub fn leave(&mut self, call: &Call) {
        if let Some(snapshot) = self.diagnostics.call.as_mut() {
            snapshot.report.ended = true;
            if snapshot.report.end.is_none() {
                snapshot.report.end = Some(crate::diagnostics::End::Left);
            }
        }
        #[cfg(target_os = "android")]
        veil_media::ffi::android::call_active(false);
        self.stop_sharing();
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

    fn stop_sharing(&mut self) {
        self.share.stop();
        self.share.set_tap(None);
        self.share.sound.set_tap(None);
    }

    /// Drops the pictures of shares that have ended.
    pub fn keep_screens(&mut self, sharers: &[u32]) {
        self.remote.retain(|key, _| {
            !crate::engine::is_screen(*key)
                || sharers
                    .iter()
                    .any(|id| crate::engine::screen_key(*id) == *key)
        });
    }

    /// Uploads pictures that arrived for other people's tiles.
    pub fn upload_remote(&mut self, pictures: Vec<(u32, crate::convert::Rgba)>) {
        for (id, picture) in pictures {
            self.framing
                .offer(id, picture.width, picture.height, &picture.pixels);
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
        #[cfg(target_os = "android")]
        veil_media::ffi::android::call_active(false);
        self.stop_sharing();
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
            let you = crate::framing::YOU;
            self.framing
                .offer(you, picture.width, picture.height, &picture.pixels);
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
        if !self.share.is_sharing() {
            self.share_view = None;
        } else if let Some(picture) = self.share.take_sending() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [picture.width, picture.height],
                &picture.pixels,
            );
            match &mut self.share_view {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    let texture = ctx.load_texture("sharing", image, TextureOptions::LINEAR);
                    self.share_view = Some(texture);
                }
            }
        }
        let target = self.media.level();
        if target > crate::engine::SPEAKING {
            self.loud_at = Some(Instant::now());
        }
        // Fast attack, slow release, like a hardware meter.
        let rate = if target > self.level { 0.6 } else { 0.15 };
        self.level += (target - self.level) * rate;
        crate::screens::effects::take_imports(self, ctx);
        self.remember_prefs(ctx);
    }

    /// Saves device, filter, blur, mirror and volume changes from any screen, once settled.
    fn remember_prefs(&mut self, ctx: &egui::Context) {
        let now = crate::preferences::Prefs {
            lobby_alerts: Some(self.lobby_alerts),
            face_framing: Some(self.framing.enabled()),
            ..crate::preferences::Prefs::current(&self.media, &self.prefs.saved, &self.devices)
        };
        #[cfg(target_os = "android")]
        let now = crate::preferences::Prefs {
            display_name: self.name.clone(),
            ..now
        };
        if self.prefs.observe(now, Instant::now()) {
            ctx.request_repaint_after(crate::preferences::SETTLE);
        }
    }

    /// Tells you when someone starts waiting in your lobby, and clears that once the lobby
    /// empties (`lobby_alert.rs`). Runs from [`eframe::App::logic`], so it works while the
    /// window is minimised or hidden, when eframe skips the interface.
    fn alert_lobby(&mut self, ctx: &egui::Context) {
        let lobby = match &self.screen {
            Screen::Call(view) => view.waiting(),
            _ => Vec::new(),
        };
        let lobby: Vec<(u32, &str)> = lobby.iter().map(|(id, n)| (*id, n.as_str())).collect();
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        self.lobby_alert
            .apply(ctx, &lobby, focused, self.lobby_alerts, crate::WINDOW_TITLE);
    }

    /// What you are sending while you share.
    pub fn share_video(&self) -> Option<(egui::TextureId, [usize; 2])> {
        self.share_view.as_ref().map(|t| (t.id(), t.size()))
    }

    /// Your camera texture and its size, for a tile. It shows exactly what you send.
    pub fn preview_video(&self) -> Option<(egui::TextureId, [usize; 2])> {
        self.preview.as_ref().map(|t| (t.id(), t.size()))
    }
}

impl eframe::App for VeilApp {
    /// Runs every pass, and also while the window is minimised or hidden, when eframe skips
    /// [`Self::ui`]: the demo's timeline and the lobby alert must not wait for the host to
    /// look.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Screen::Call(view) = &mut self.screen {
            view.logic(ctx, Instant::now());
        }
        self.alert_lobby(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(not(target_os = "android"))]
        crate::window_size::apply_once(&ctx);
        #[cfg(target_os = "android")]
        {
            let (top, bottom) = veil_media::ffi::android::insets();
            let scale = ctx.pixels_per_point();
            let font_scale = veil_media::ffi::android::font_scale();
            let font_id = egui::Id::unique("android-font-scale");
            if ctx.data(|data| data.get_temp::<f32>(font_id)) != Some(font_scale) {
                crate::mobile::apply_style(&ctx, font_scale);
                ctx.data_mut(|data| data.insert_temp(font_id, font_scale));
            }
            // Open the keyboard when a text field takes focus, close it when none has it.
            let typing = ctx.text_edit_focused();
            let keyboard_id = egui::Id::unique("android-keyboard");
            if ctx
                .data(|data| data.get_temp::<bool>(keyboard_id))
                .unwrap_or(false)
                != typing
            {
                veil_media::ffi::android::keyboard(typing);
                ctx.data_mut(|data| data.insert_temp(keyboard_id, typing));
            }
            let (left, right) = veil_media::ffi::android::side_insets();
            if left > 0 {
                egui::Panel::left("android-left-inset")
                    .exact_size(left as f32 / scale)
                    .frame(egui::Frame::new().fill(crate::theme::PAGE))
                    .show_separator_line(false)
                    .show(ui, |_| {});
            }
            if right > 0 {
                egui::Panel::right("android-right-inset")
                    .exact_size(right as f32 / scale)
                    .frame(egui::Frame::new().fill(crate::theme::PAGE))
                    .show_separator_line(false)
                    .show(ui, |_| {});
            }
            egui::Panel::top("android-status-bar")
                .exact_size(top as f32 / scale)
                .frame(egui::Frame::new().fill(crate::theme::PAGE))
                .show_separator_line(false)
                .show(ui, |_| {});
            egui::Panel::bottom("android-navigation-bar")
                .exact_size(bottom as f32 / scale)
                .frame(egui::Frame::new().fill(crate::theme::PAGE))
                .show_separator_line(false)
                .show(ui, |_| {});
            if matches!(self.screen, Screen::Home)
                && let Some(invite) = veil_media::ffi::android::take_invite()
            {
                self.invite = invite;
            }
        }
        #[cfg(target_os = "android")]
        if veil_media::ffi::android::back_requested() {
            if self.diagnostics.open {
                self.diagnostics.open = false;
            } else {
                match &mut self.screen {
                    Screen::Call(view) if view.mobile_invite => view.mobile_invite = false,
                    Screen::Call(view) if view.panel.is_some() => view.panel = None,
                    Screen::Call(_) => {
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::unique("mobile-leave"), true);
                        });
                    }
                    Screen::Home => veil_media::ffi::android::background(),
                    _ => self.go_home(),
                }
            }
        }
        self.update_media(&ctx);
        match std::mem::replace(&mut self.screen, Screen::Home) {
            Screen::Home => {
                self.screen = Screen::Home;
                if cfg!(target_os = "android") {
                    crate::mobile::home(self, ui);
                } else {
                    crate::screens::home::show(self, ui);
                }
            }
            Screen::PreJoin { host } => {
                self.screen = Screen::PreJoin { host };
                if cfg!(target_os = "android") {
                    crate::mobile::prejoin(self, ui, host);
                } else {
                    crate::screens::prejoin::show(self, ui, host);
                }
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
                if cfg!(target_os = "android") {
                    crate::mobile::ended(self, ui);
                } else {
                    crate::screens::home::ended(self, ui);
                }
            }
        }
        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::E)
        }) {
            self.diagnostics.open();
        }
        self.diagnostics.ui(&ctx);
    }
}
