//! The call: a top bar (title, encryption, timer, layout), the stage of video tiles, the
//! control bar, and one side panel at a time (`panels.rs`).
//!
//! Invariants:
//! - Muting closes the microphone and stopping video closes the camera (see `media.rs`), so
//!   nothing is captured while you appear off.
//! - Shortcuts: Ctrl+Shift+M mutes or unmutes, Ctrl+Shift+O starts or stops video.
//! - A demo call always shows its banner.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Modifiers, Rect, RichText,
    pos2, vec2,
};

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::layout;
use crate::model::{Call, Participant, clock};
use crate::screens::panels::{self, Panel};
use crate::theme::{self, GAP, MUTED, PAGE, SURFACE};
use crate::widgets::{self, ControlState, Tile, Tone, control, pill};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Gallery,
    Speaker,
}

pub struct CallView {
    pub call: Call,
    pub panel: Option<Panel>,
    pub view: View,
    pub draft: String,
    notice: Option<(String, Instant)>,
}

impl CallView {
    pub fn new(call: Call) -> Self {
        Self {
            call,
            panel: None,
            view: View::Gallery,
            draft: String::new(),
            notice: None,
        }
    }

    pub fn notify(&mut self, text: &str) {
        self.notice = Some((text.to_string(), Instant::now()));
    }

    /// Draws the call. Returns false when you leave.
    pub fn show(&mut self, app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
        let now = Instant::now();
        if self.call.demo {
            crate::demo::tick(&mut self.call, now);
        }
        if self.panel == Some(Panel::Chat) {
            self.call.unread = 0;
        }
        self.sync_you(app);
        self.shortcuts(app, ui);
        top_bar(self, ui, now);
        let stay = control_bar(self, app, ui);
        if let Some(panel) = self.panel {
            panels::show(self, ui, panel, now);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PAGE)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| self.stage(app, ui, now));
        // The timer and the demo speakers move on their own.
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        stay
    }

    /// Your own tile follows the devices and the microphone level.
    fn sync_you(&mut self, app: &VeilApp) {
        let speaking = app.media.mic_on() && app.level > 0.45;
        if let Some(you) = self.call.you_mut() {
            you.muted = !app.media.mic_on();
            you.camera = app.media.camera_on();
            you.speaking = speaking;
        }
    }

    fn shortcuts(&mut self, app: &mut VeilApp, ui: &egui::Ui) {
        let combo = Modifiers::COMMAND | Modifiers::SHIFT;
        let (mute, video) =
            ui.input_mut(|i| (i.consume_key(combo, Key::M), i.consume_key(combo, Key::O)));
        if mute {
            toggle_mic(app);
        }
        if video {
            toggle_camera(app);
        }
    }

    fn stage(&mut self, app: &VeilApp, ui: &mut egui::Ui, now: Instant) {
        let mut area = ui.available_rect_before_wrap();
        if self.call.demo {
            area.min.y += banner(ui, area) + GAP;
        }
        let people = &self.call.participants;
        match self.view {
            View::Gallery => {
                for (person, rect) in people.iter().zip(layout::gallery(people.len(), area, GAP)) {
                    draw(app, ui, rect, person);
                }
            }
            View::Speaker => {
                let focus = self.call.focus().map(|p| p.id);
                let others: Vec<&Participant> =
                    people.iter().filter(|p| Some(p.id) != focus).collect();
                let (stage, strip) = layout::speaker(others.len(), area, GAP);
                if let Some(person) = people.iter().find(|p| Some(p.id) == focus) {
                    draw(app, ui, stage, person);
                }
                for (person, rect) in others.iter().zip(strip) {
                    draw(app, ui, rect, person);
                }
            }
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

fn draw(app: &VeilApp, ui: &egui::Ui, rect: Rect, person: &Participant) {
    let video = if person.is_you && person.camera {
        app.preview_video()
    } else {
        None
    };
    let tile = Tile {
        name: &person.name,
        video,
        muted: person.muted,
        speaking: person.speaking,
        hand_raised: person.hand_raised,
        is_you: person.is_you,
    };
    widgets::tile(ui, rect, &tile);
}

/// The demo warning across the top of the stage. Returns its height.
fn banner(ui: &egui::Ui, area: Rect) -> f32 {
    let rect = Rect::from_min_size(area.min, vec2(area.width(), 32.0));
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(6),
        theme::WARN.gamma_multiply(0.18),
    );
    Icon::Info.paint(
        painter,
        pos2(rect.left() + 18.0, rect.center().y),
        14.0,
        theme::WARN,
    );
    painter.text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Demo call: the other people are simulated. Your camera and microphone are real and stay on this device.",
        FontId::proportional(13.0),
        theme::WARN,
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

fn top_bar(view: &mut CallView, ui: &mut egui::Ui, now: Instant) {
    egui::Panel::top("call_top")
        .exact_size(52.0)
        .frame(
            egui::Frame::new()
                .fill(SURFACE)
                .inner_margin(egui::Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(&view.call.title).font(theme::bold(15.0)));
                ui.label(RichText::new(clock(view.call.elapsed(now))).color(MUTED));
                let secure = ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{}  End-to-end encrypted", Icon::Lock.text()))
                                .color(theme::SPEAKING),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Show safety numbers and how each person is connected");
                if secure.clicked() {
                    view.panel = toggle(view.panel, Panel::Security);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (icon, label, next) = match view.view {
                        View::Gallery => (Icon::Speaker, "Speaker view", View::Speaker),
                        View::Speaker => (Icon::Grid, "Gallery view", View::Gallery),
                    };
                    if widgets::icon_button(ui, icon, label).clicked() {
                        view.view = next;
                    }
                    ui.label(
                        RichText::new(format!("{} people", view.call.participants.len()))
                            .color(MUTED),
                    );
                });
            });
        });
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
            let controls_w = 7.0 * 68.0 + 6.0 * 6.0;
            let start = ((ui.available_width() - controls_w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(start);
                device_controls(view, app, ui);
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

fn device_controls(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) {
    let mic_on = app.media.mic_on();
    let state = ControlState {
        off: !mic_on,
        ..Default::default()
    };
    let label = if mic_on { "Mute" } else { "Unmute" };
    if control(ui, Icon::Mic, label, "Microphone (Ctrl+Shift+M)", state).clicked() {
        toggle_mic(app);
    }
    let cam_on = app.media.camera_on();
    let state = ControlState {
        off: !cam_on,
        ..Default::default()
    };
    let label = if cam_on { "Stop video" } else { "Start video" };
    if control(ui, Icon::Video, label, "Camera (Ctrl+Shift+O)", state).clicked() {
        toggle_camera(app);
    }
    if control(
        ui,
        Icon::Share,
        "Share",
        "Share your screen",
        ControlState::default(),
    )
    .clicked()
    {
        view.notify("Screen sharing is not built yet.");
    }
    let raised = view.call.you().is_some_and(|p| p.hand_raised);
    let state = ControlState {
        selected: raised,
        ..Default::default()
    };
    let label = if raised { "Lower" } else { "Raise" };
    if control(ui, Icon::Hand, label, "Raise or lower your hand", state).clicked() {
        if let Some(you) = view.call.you_mut() {
            you.hand_raised = !raised;
        }
    }
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

fn toggle(open: Option<Panel>, panel: Panel) -> Option<Panel> {
    if open == Some(panel) {
        None
    } else {
        Some(panel)
    }
}

fn toggle_mic(app: &mut VeilApp) {
    let on = !app.media.mic_on();
    app.want_mic = on;
    app.media.set_mic(on);
}

fn toggle_camera(app: &mut VeilApp) {
    let on = !app.media.camera_on();
    app.want_camera = on;
    app.media.set_camera(on);
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
