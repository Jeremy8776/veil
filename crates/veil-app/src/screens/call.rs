//! The call: a top bar (title, encryption, timer, layout), the stage of video tiles, the
//! control bar, and one side panel at a time (`panels.rs`).
//!
//! Invariants:
//! - Muting closes the microphone and stopping video closes the camera (see `media.rs`), so
//!   nothing is captured while you appear off.
//! - Shortcuts: Ctrl+Shift+M mutes or unmutes, Ctrl+Shift+O starts or stops video,
//!   Ctrl+Shift+D shows or hides the call stats (`stats.rs`, off by default).
//! - A demo call always shows its banner.
//! - The host's invite is up front: an Invite button in the top bar, and a card on the stage
//!   while nobody else has joined.
//! - A real call reads the engine once per frame, before drawing, and hands it the host's
//!   decisions from the model's outbox. It ends when the engine says so.

use crate::screens::speaker::{self, Seat};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Modifiers, Rect, pos2, vec2,
};

use crate::app::VeilApp;
use crate::engine::Engine;
use crate::icons::Icon;
use crate::layout;
use crate::model::{Call, CallStatus};
use crate::screens::panels::{self, Panel};
use crate::screens::security_panel::Verifying;
use crate::screens::share::SharePicker;
use crate::screens::share_bar;
use crate::screens::{call_top, controls, invite, stats};
use crate::theme::{self, GAP, MUTED, PAGE, SURFACE};
use crate::widgets::{ControlState, Tone, control, pill};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Everyone at once, in justified rows.
    Group,
    /// The active speaker large, everyone else small in a strip.
    Focus,
}

pub struct CallView {
    pub call: Call,
    pub panel: Option<Panel>,
    pub view: View,
    pub draft: String,
    /// The person being checked in the Security panel.
    pub verifying: Option<Verifying>,
    pub(crate) notice: Option<(String, Instant)>,
    pub(crate) mobile_invite: bool,
    /// Whether the blur-off notice has been shown as a toast, so it shows once.
    blur_notice_shown: bool,
    /// The call stats overlay is on.
    pub stats: bool,
    /// The Share picker, while it is open.
    pub picker: Option<SharePicker>,
    /// Who is sharing their screen, by participant id.
    pub(super) sharers: Vec<u32>,
    /// Who is sending computer sound with their share, by participant id.
    pub(super) sounding: Vec<u32>,
    pub(crate) tile_motion: layout::Motion,
    /// Who holds the stage in Focus view, kept between frames for the speaker hold.
    pub(crate) focus: Option<speaker::Focus>,
    /// The host's reminder about someone who has waited a while (`lobby_reminder.rs`).
    pub(crate) reminder: crate::lobby_reminder::Reminder,
    /// The host's Lobby alerts switch, copied to and from `VeilApp::lobby_alerts` each frame.
    pub(crate) lobby_alerts: bool,
    /// The real call; `None` in a demo. Dropping it leaves the call.
    engine: Option<Engine>,
}

impl CallView {
    pub fn new(call: Call, engine: Option<Engine>) -> Self {
        Self {
            engine,
            call,
            panel: None,
            view: View::Group,
            draft: String::new(),
            verifying: None,
            notice: None,
            mobile_invite: false,
            blur_notice_shown: false,
            stats: false,
            picker: None,
            sharers: Vec::new(),
            sounding: Vec::new(),
            tile_motion: layout::Motion::default(),
            focus: None,
            reminder: crate::lobby_reminder::Reminder::default(),
            lobby_alerts: true,
        }
    }

    /// Lets someone in from the lobby.
    pub(crate) fn admit(&mut self, id: u32) {
        self.call.admit(id);
        self.notify("Admitted. A new key epoch started.");
    }

    pub fn notify(&mut self, text: &str) {
        self.notice = Some((text.to_string(), Instant::now()));
    }

    /// Moves the stage to whoever is speaking, with the speaker hold. Runs in both views,
    /// so switching to Focus opens on whoever is talking.
    pub(crate) fn update_focus(&mut self) {
        let seats: Vec<_> = self.call.participants.iter().map(Seat::from).collect();
        self.focus = speaker::pick(self.focus, &seats, Instant::now());
    }

    /// Whether anyone, you included, is sharing a screen.
    pub(crate) fn sharing(&self, app: &VeilApp) -> bool {
        !self.sharers.is_empty() || app.share_video().is_some()
    }

    pub(crate) fn watch_tiles(&self, tiles: Vec<(u32, f32)>) {
        if let Some(engine) = &self.engine {
            engine.watch(tiles);
        }
    }

    /// Puts the invite link on the clipboard and says so.
    pub fn copy_invite(&mut self, ctx: &egui::Context) {
        if self.call.invite.is_empty() {
            self.notify("The invite link is not ready yet.");
        } else {
            ctx.copy_text(self.call.invite.clone());
            self.notify("Invite link copied");
        }
    }

    /// Puts the full invite link on the clipboard, for a guest the short one cannot reach.
    pub fn copy_full_invite(&mut self, ctx: &egui::Context) {
        ctx.copy_text(self.call.invite_full.clone());
        self.notify("Full invite link copied");
    }

    /// The host is alone in a real call: show the invite where it cannot be missed.
    fn waiting_for_guests(&self) -> bool {
        self.call.is_host
            && !self.call.demo
            && self.call.status == CallStatus::Live
            && self.call.participants.len() < 2
    }

    /// Draws the call. Returns false when you leave.
    pub fn show(&mut self, app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
        let now = Instant::now();
        if self.call.demo {
            crate::demo::tick(&mut self.call, now);
        }
        if let Some(engine) = &self.engine {
            let was_live = matches!(
                self.call.status,
                CallStatus::Live | CallStatus::Reconnecting
            );
            for action in self.call.outbox.drain(..) {
                engine.act(action);
            }
            engine.sync(&mut self.call, now);
            let mut snapshot = engine.diagnostics();
            snapshot.camera = app.media.camera_evidence();
            snapshot.microphone = app.media.microphone_evidence();
            let (camera, mic) = {
                #[cfg(target_os = "android")]
                {
                    veil_media::ffi::android::permission_state()
                }
                #[cfg(not(target_os = "android"))]
                {
                    (true, true)
                }
            };
            snapshot.camera = snapshot.camera.with_permission(app.want_camera, camera);
            snapshot.microphone = snapshot.microphone.with_permission(app.want_mic, mic);
            app.diagnostics.call = Some(snapshot);
            app.upload_remote(engine.take_pictures());
            self.sharers = engine.sharers();
            self.sounding = engine.sounding();
            app.keep_screens(&self.sharers);
            // A guest's timer starts when the host lets them in, not in the lobby.
            if !was_live && self.call.status == CallStatus::Live && !self.call.is_host {
                self.call.started = now;
            }
        }
        if matches!(self.call.status, CallStatus::Ended(_)) {
            crate::invite_qr::clear(ui.ctx());
            return false;
        }
        if self.panel == Some(Panel::Chat) {
            self.call.unread = 0;
        }
        if app.share.take_stopped() {
            self.notify("Screen sharing stopped.");
        }
        if let Some(why) = app.share.sound.take_error() {
            self.notify(&why);
        }
        self.sync_you(app);
        if cfg!(target_os = "android") {
            let stay = crate::mobile::call(self, app, ui);
            if self.call.is_host && !self.call.demo {
                crate::invite_qr::show(ui.ctx(), &self.call.invite_full);
            }
            if !stay {
                crate::invite_qr::clear(ui.ctx());
            }
            return stay;
        }
        self.shortcuts(app, ui);
        call_top::show(self, app, ui, now);
        let stay = control_bar(self, app, ui);
        self.lobby_alerts = app.lobby_alerts;
        if let Some(panel) = self.panel {
            panels::show(self, ui, panel, now);
        }
        app.lobby_alerts = self.lobby_alerts;
        crate::lobby_reminder::show(self, ui.ctx(), now);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PAGE)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| self.stage(app, ui, now));
        app.share.tick();
        if app.share.take_change_asked() && self.picker.is_none() {
            self.picker = Some(SharePicker::open(app));
        }
        if let Some(mut picker) = self.picker.take()
            && picker.show(app, ui)
        {
            self.picker = Some(picker);
        }
        share_bar::show(app, ui.ctx());
        if self.call.is_host && !self.call.demo {
            crate::invite_qr::show(ui.ctx(), &self.call.invite_full);
        }
        // The timer and the demo speakers move on their own.
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        if !stay {
            crate::invite_qr::clear(ui.ctx());
        }
        stay
    }

    /// Your own tile follows the devices and the microphone level.
    fn sync_you(&mut self, app: &VeilApp) {
        let speaking =
            app.media.mic_on() && crate::engine::still_speaking(app.loud_at, Instant::now());
        if let Some(you) = self.call.you_mut() {
            you.muted = !app.media.mic_on();
            you.camera = app.media.camera_on();
            you.speaking = speaking;
        }
    }

    fn shortcuts(&mut self, app: &mut VeilApp, ui: &egui::Ui) {
        let combo = Modifiers::COMMAND | Modifiers::SHIFT;
        let (mute, video, stats) = ui.input_mut(|i| {
            let mut key = |k| i.consume_key(combo, k);
            (key(Key::M), key(Key::O), key(Key::D))
        });
        if mute {
            controls::toggle_mic(app);
        }
        if video {
            controls::toggle_camera(app);
        }
        if stats {
            self.stats = !self.stats;
        }
    }

    fn stage(&mut self, app: &VeilApp, ui: &mut egui::Ui, now: Instant) {
        match app.media.blur_notice() {
            Some(text) if !self.blur_notice_shown => {
                self.notify(&text);
                self.blur_notice_shown = true;
            }
            None => self.blur_notice_shown = false,
            Some(_) => {}
        }
        let mut area = ui.available_rect_before_wrap();
        if let Some((text, colour)) = self.banner_text() {
            area.min.y += banner(ui, area, text, colour) + GAP;
        }
        let waiting = self.waiting_for_guests();
        let waiting_area = super::bento::show(self, app, ui, area, waiting);
        if self.stats {
            let (measured, levels) = self.engine.as_ref().map(Engine::stats).unwrap_or_default();
            let shown = stats::Shown {
                call: &self.call,
                stats: &measured,
                levels: &levels,
                mic: app.media.mic_on().then_some(app.level),
            };
            stats::overlay(ui, area, &shown);
        }
        if let Some(invite_area) = waiting_area {
            invite::card(self, ui, invite_area);
        }
        if let Some((text, at)) = &self.notice {
            if now.duration_since(*at) < Duration::from_secs(3) {
                toast(ui, area, text);
            } else {
                self.notice = None;
            }
        }
    }
}

impl CallView {
    /// The line across the top of the stage, if the call needs one.
    fn banner_text(&self) -> Option<(&'static str, Color32)> {
        let alone = self.call.participants.len() < 2;
        match &self.call.status {
            _ if self.call.demo => Some(("Demo: other people are simulated.", theme::WARN)),
            CallStatus::Connecting if self.call.is_host => Some(("Starting the call…", MUTED)),
            // Direct, then hole punching, then the relay: up to about 30 s over the internet.
            CallStatus::Connecting => {
                Some(("Reaching the host. This can take up to 30 seconds.", MUTED))
            }
            CallStatus::Lobby => Some(("Waiting for the host to let you in.", MUTED)),
            CallStatus::Reconnecting => Some(("Reconnecting to the host…", MUTED)),
            CallStatus::Live if alone && self.call.is_host => None,
            CallStatus::Live if alone => Some(("Connecting to the others…", MUTED)),
            CallStatus::Live | CallStatus::Ended(_) => None,
        }
    }
}

/// A notice across the top of the stage: the demo warning, or where the call stands.
/// Returns its height.
fn banner(ui: &egui::Ui, area: Rect, text: &str, colour: Color32) -> f32 {
    let rect = Rect::from_min_size(area.min, vec2(area.width(), 32.0));
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), colour.gamma_multiply(0.18));
    Icon::Info.paint(
        painter,
        pos2(rect.left() + 18.0, rect.center().y),
        14.0,
        colour,
    );
    painter.text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        colour,
    );
    rect.height()
}

fn toast(ui: &egui::Ui, area: Rect, text: &str) {
    let painter = ui.painter();
    let galley =
        painter.layout_no_wrap(text.to_string(), FontId::proportional(13.5), Color32::WHITE);
    let size = galley.size() + vec2(28.0, 18.0);
    let rect = Rect::from_center_size(pos2(area.center().x, area.bottom() - 40.0), size);
    painter.rect_filled(rect, CornerRadius::same(8), Color32::from_black_alpha(220));
    painter.galley(rect.min + vec2(14.0, 9.0), galley, Color32::WHITE);
}

/// The bar of call controls. Returns false when you leave.
fn control_bar(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
    let mut stay = true;
    egui::Panel::bottom("call_controls")
        .exact_size(82.0)
        .frame(
            egui::Frame::new()
                .fill(SURFACE)
                .inner_margin(egui::Margin::symmetric(16, 12)),
        )
        .show(ui, |ui| {
            let leave_w = 110.0;
            let buttons = controls::CONTROLS + 3;
            // The arrows join their buttons, so they add no gaps.
            let gaps = (buttons - 1) as f32;
            let controls_w =
                buttons as f32 * 68.0 + controls::MENUS as f32 * controls::MENU_W + gaps * 6.0;
            let start = ((ui.available_width() - controls_w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(start);
                controls::show(view, app, ui);
                ui.add_space(4.0);
                panel_controls(view, ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if pill(ui, Some(Icon::Leave), "Leave", Tone::Danger, leave_w).clicked() {
                        stay = false;
                    }
                });
            });
        });
    stay
}

fn panel_controls(view: &mut CallView, ui: &mut egui::Ui) {
    let buttons = [
        (
            Panel::Chat,
            Icon::Chat,
            "Chat",
            "Open the chat",
            view.call.unread,
        ),
        (
            Panel::People,
            Icon::People,
            "People",
            "People and the lobby",
            view.call.lobby.len(),
        ),
        (
            Panel::Security,
            Icon::Shield,
            "Security",
            "Safety numbers and connections",
            0,
        ),
    ];
    for (panel, icon, label, hint, badge) in buttons {
        let state = ControlState {
            selected: view.panel == Some(panel),
            badge,
            ..Default::default()
        };
        if control(ui, icon, label, hint, state).clicked() {
            view.panel = toggle(view.panel, panel);
        }
    }
}

pub fn toggle(open: Option<Panel>, panel: Panel) -> Option<Panel> {
    if open == Some(panel) {
        None
    } else {
        Some(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_a_panel_opens_switches_and_closes() {
        assert_eq!(toggle(None, Panel::Chat), Some(Panel::Chat));
        assert_eq!(
            toggle(Some(Panel::Chat), Panel::People),
            Some(Panel::People)
        );
        assert_eq!(toggle(Some(Panel::People), Panel::People), None);
    }
}
