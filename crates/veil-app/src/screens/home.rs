//! The first screen (your name, start a call, join one with an invite link, or try the demo)
//! and the screen after a call ends.
//!
//! Invariants:
//! - An invite is checked here (signature, expiry) before any device or network starts, so a
//!   bad link is reported where it was pasted. A short link is checked for its form only: the
//!   invite it stands for is fetched and verified when the call starts.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Layout, RichText, TextEdit};
use veil_security::short_invite::InviteLink;

use crate::app::VeilApp;
use crate::icons::Icon;
use crate::model::clock;
use crate::theme::{self, FORM_CONTROL_WIDTH, FORM_WIDTH, HELP_WIDTH, MUTED, PAGE_COLUMN_GAP};
use crate::widgets::{Tone, card, pill};

const COLUMN: f32 = 440.0;

/// Help follows the control in use without changing any call or device state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum HomeHelp {
    #[default]
    Welcome,
    Name,
    NewCall,
    Join,
    Demo,
    Connection,
    DeviceKey,
    NewKey,
    Logs,
    Updates,
    SetupSelector,
    SetupCalls(bool),
    SetupJoining(bool),
    SetupServers,
    SetupHosting(bool),
    SetupManage,
}

impl HomeHelp {
    pub(super) fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::SetupSelector => (
                "Active setup",
                "Switch to a saved setup for your next call. Manage connections to add or edit setups.",
            ),
            Self::SetupCalls(true) => (
                "Local calls",
                "Calls stay on connected local networks. Internet servers and router port opening are disabled.",
            ),
            Self::SetupCalls(false) => (
                "Internet and local calls",
                "Internet and local calls are enabled. This shows your settings; it does not confirm reachability.",
            ),
            Self::SetupJoining(true) => (
                "Address hidden when joining",
                "The relay hides your IP from participants when joining. The relay still sees it. Hosting is unaffected.",
            ),
            Self::SetupJoining(false) => (
                "Direct joining",
                "Direct connections are allowed when joining. Other participants may see your IP address.",
            ),
            Self::SetupServers => (
                "Connection servers",
                "Built-in helpers start when you host. A server check verifies reachability, not permission to host.",
            ),
            Self::SetupHosting(true) => (
                "Check local hosting",
                "Local hosting has not been verified. Join from another device on this network to check it.",
            ),
            Self::SetupHosting(false) => (
                "Check hosting",
                "Hosting has not been verified. Join from a second device to check the incoming path.",
            ),
            Self::SetupManage => (
                "Manage connections",
                "Add, rename, remove or test saved setups. Saving a setup makes it active for your next call.",
            ),
            Self::Welcome => (
                "Veil",
                "Video calls, screen sharing and chat, with no accounts or telemetry. Your device creates the keys, and the people in your call hold them.",
            ),
            Self::Name => (
                "Your name",
                "This is the name other people see in your call. You need a name before starting or joining.",
            ),
            Self::NewCall => (
                "Start a call",
                "Check your camera and microphone, then start the call and share an invite link. People you invite wait in the lobby until you admit them.",
            ),
            Self::Join => (
                "Join with an invite",
                "Paste the invite link you were sent. Veil checks it before opening your camera or connecting. You can check your camera and microphone before joining the host's lobby.",
            ),
            Self::Demo => (
                "Try a demo call",
                "Explore the interface with simulated people. Nothing is sent, and you do not need anyone else to join.",
            ),
            Self::Connection => (
                "Connection",
                "Choose Local only and nearby discovery, your own servers, a fixed call port, or address privacy. These settings apply to your next call.",
            ),
            Self::DeviceKey => (
                "Your device key",
                "Copy this public key to share with someone who runs a veil-node, so they can let you host calls through it. It identifies this device and cannot be used to sign in as you.",
            ),
            Self::NewKey => (
                "Replace your device key",
                "A new key means people who verified you see a new device. Use it if you think your key was copied. The first click asks for confirmation; the second replaces the key.",
            ),
            Self::Logs => (
                "Logs",
                "Open the folder with Veil's log files. If something goes wrong, use veil.log, or veil.previous.log if Veil froze or closed. They hold states, timings and errors: no names, chat or invite links.",
            ),
            Self::Updates => (
                "Check for updates",
                "Asks GitHub once per start whether a newer Veil is out. GitHub sees your IP address. Nothing is downloaded unless you choose Install.",
            ),
        }
    }
}

pub(super) type HomeHelpState = crate::widgets::HelpState<HomeHelp>;

pub(super) fn track_help(response: &egui::Response, help: HomeHelp, state: &mut HomeHelpState) {
    state.track(response, help);
}

pub fn show(app: &mut VeilApp, ui: &mut egui::Ui) {
    if let Some(Err(problem)) = app.log_opener.poll() {
        app.home_error = Some(problem);
    }
    let mut help_state = HomeHelpState::default();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(32))
        .show(ui, |ui| {
            let compact_height = ui.available_height() < 800.0;
            let height_key = ui.make_persistent_id("home-content-height");
            let last_height: f32 = ui.ctx().data(|d| d.get_temp(height_key)).unwrap_or(0.0);
            let top = ((ui.available_height() - last_height) * 0.5).max(0.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(top);
                let show_help = ui.available_width() >= FORM_WIDTH + PAGE_COLUMN_GAP + HELP_WIDTH;
                let width = ui.available_width().min(if show_help {
                    FORM_WIDTH + PAGE_COLUMN_GAP + HELP_WIDTH
                } else {
                    FORM_WIDTH
                });
                let content = ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 0.0),
                        Layout::top_down(Align::Center),
                        |ui| {
                            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                crate::logo::show(
                                    ui,
                                    if compact_height { 128.0 } else { 160.0 },
                                    theme::PAGE,
                                );
                            });
                            ui.add_space(if compact_height { 16.0 } else { 24.0 });
                            update_banner(app, ui, width - 42.0);
                            let mut helper_rect = None;
                            if show_help {
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = PAGE_COLUMN_GAP;
                                    let form = ui.allocate_ui_with_layout(
                                        egui::vec2(FORM_WIDTH, 0.0),
                                        Layout::top_down(Align::Center),
                                        |ui| {
                                            ui.spacing_mut().item_spacing.x = theme::GAP;
                                            home_form(app, ui, FORM_WIDTH, &mut help_state, false)
                                        },
                                    );
                                    helper_rect = Some(
                                        ui.allocate_exact_size(
                                            egui::vec2(HELP_WIDTH, form.inner.rect.height()),
                                            egui::Sense::hover(),
                                        )
                                        .0,
                                    );
                                });
                            } else {
                                home_form(app, ui, width, &mut help_state, true);
                                ui.add_space(8.0);
                                egui::CollapsingHeader::new("Your setup").show(ui, |ui| {
                                    if let Some(action) = super::home_setup::summary(
                                        ui,
                                        &app.connection,
                                        &mut help_state,
                                        true,
                                    ) {
                                        super::home_setup::apply(app, action);
                                    }
                                });
                            }
                            ui.add_space(20.0);
                            ui.label(RichText::new("Tools").font(theme::bold(16.0)));
                            ui.add_space(4.0);
                            crate::screens::home_tools::show(app, ui, width, &mut help_state);
                            // Draw help after all controls have reported hover/focus, in
                            // the space reserved alongside the form. No frame of delay.
                            if let Some(rect) = helper_rect {
                                let mut helper = ui.new_child(
                                    egui::UiBuilder::new()
                                        .id_salt("home-helper")
                                        .max_rect(rect)
                                        .layout(Layout::top_down(Align::Center)),
                                );
                                if let Some(action) = super::home_setup::helper_card(
                                    &mut helper,
                                    &mut help_state,
                                    rect.height(),
                                    &app.connection,
                                ) {
                                    super::home_setup::apply(app, action);
                                }
                            }
                        },
                    );
                });
                let height = content.response.rect.height();
                if (height - last_height).abs() > 0.5 {
                    ui.ctx().data_mut(|d| d.insert_temp(height_key, height));
                    ui.ctx().request_repaint();
                }
            });
        });
}

pub(super) fn helper_text(ui: &mut egui::Ui, help: HomeHelp) {
    let (title, detail) = help.text();
    let welcome = matches!(help, HomeHelp::Welcome);
    ui.label(RichText::new(title).font(theme::bold(if welcome { 28.0 } else { 20.0 })));
    ui.add_space(8.0);
    ui.label(RichText::new(detail).color(MUTED));
}

fn home_form(
    app: &mut VeilApp,
    ui: &mut egui::Ui,
    width: f32,
    help: &mut HomeHelpState,
    show_tooltips: bool,
) -> egui::Response {
    card()
        .show(ui, |ui| {
            ui.set_width((width - 42.0).max(0.0));
            let label = ui.label(RichText::new("Your name").color(MUTED));
            let name = ui
                .add(
                    TextEdit::singleline(&mut app.name)
                        .align(egui::Align2::CENTER_CENTER)
                        .hint_text("How others see you")
                        .char_limit(veil_call::app_msg::MAX_NAME_CHARS)
                        .desired_width(ui.available_width().min(FORM_CONTROL_WIDTH)),
                )
                .labelled_by(label.id);
            track_form_help(&name, HomeHelp::Name, help, show_tooltips);
            track_form_help(&label, HomeHelp::Name, help, show_tooltips);
            ui.add_space(12.0);
            start_section(app, ui, help, show_tooltips);
            ui.add_space(20.0);
            ui.separator();
            ui.add_space(12.0);
            join_section(app, ui, help, show_tooltips);
            if let Some(error) = &app.home_error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(theme::DANGER_HOVER));
            }
            let keys = app.keys.clone();
            let verified = move |key: &veil_security::PublicId| {
                keys.lock().is_ok_and(|keys| keys.is_verified(key))
            };
            if let Some(invite) = app.nearby.ui(ui, verified) {
                app.joining = Some(invite);
                app.demo = false;
                start(app, false);
            }
        })
        .response
}

fn start_section(
    app: &mut VeilApp,
    ui: &mut egui::Ui,
    help: &mut HomeHelpState,
    show_tooltips: bool,
) {
    let button = pill(
        ui,
        Some(Icon::Video),
        "New call",
        Tone::Primary,
        ui.available_width().min(FORM_CONTROL_WIDTH),
    );
    track_form_help(&button, HomeHelp::NewCall, help, show_tooltips);
    if button.clicked() {
        app.demo = false;
        start(app, true);
    }
}

fn join_section(
    app: &mut VeilApp,
    ui: &mut egui::Ui,
    help: &mut HomeHelpState,
    show_tooltips: bool,
) {
    let label = ui.label(RichText::new("Join a call").font(theme::bold(18.0)));
    ui.add_space(4.0);
    let field = ui
        .add(
            TextEdit::singleline(&mut app.invite)
                .align(egui::Align2::CENTER_CENTER)
                .hint_text("Paste an invite link (veil:...)")
                .desired_width(ui.available_width().min(FORM_CONTROL_WIDTH)),
        )
        .labelled_by(label.id);
    track_form_help(&field, HomeHelp::Join, help, show_tooltips);
    let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(8.0);
    let button = pill(
        ui,
        Some(Icon::Link),
        "Join",
        Tone::Quiet,
        ui.available_width().min(FORM_CONTROL_WIDTH),
    );
    track_form_help(&button, HomeHelp::Join, help, show_tooltips);
    if button.clicked() || entered {
        match check_invite(&app.invite) {
            Ok(invite) => {
                app.joining = Some(invite);
                app.demo = false;
                start(app, false);
            }
            Err(mut problem) => {
                if matches!(app.updates.status(), crate::update::Status::Available(_)) {
                    problem.push_str(" It may be from a newer Veil: update above, then try again.");
                }
                app.home_error = Some(problem);
            }
        }
    }
}

fn track_form_help(
    response: &egui::Response,
    topic: HomeHelp,
    help: &mut HomeHelpState,
    show_tooltips: bool,
) {
    track_help(response, topic, help);
    if show_tooltips {
        response.clone().on_hover_text(topic.text().1);
    }
}

/// A small text button with no frame.
fn link(ui: &mut egui::Ui, text: &str, colour: egui::Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).small().color(colour)).frame(false))
}

/// A card above everything else while a newer Veil is out, so it is not missed: older
/// versions may not open newer invite links. Install where this build can install itself,
/// otherwise a button to the release page.
fn update_banner(app: &mut VeilApp, ui: &mut egui::Ui, width: f32) {
    use crate::update::{self, Install, Status};
    let Status::Available(version) = app.updates.status() else {
        return;
    };
    let install = app.updates.install_state();
    card()
        .stroke(egui::Stroke::new(1.5, theme::ACCENT))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(format!("Veil {version} is out")).font(theme::bold(16.0)));
            ui.label(
                RichText::new(format!(
                    "You have {}. Update so you can open the newest invite links.",
                    update::VERSION
                ))
                .small()
                .color(MUTED),
            );
            ui.add_space(8.0);
            if !app.updates.can_install() {
                let button = pill(ui, None, "Get the update", Tone::Primary, width)
                    .on_hover_text("Opens the release page.");
                if button.clicked() {
                    update::open(version);
                }
                return;
            }
            match &install {
                Install::Idle | Install::Failed(_) => {
                    let button = pill(ui, None, "Install and restart", Tone::Primary, width)
                        .on_hover_text(
                            "Downloads it from GitHub, checks Veil's release signature, then \
                             installs it. Veil restarts to finish.",
                        );
                    if button.clicked() {
                        app.updates.install(version, ui.ctx());
                    }
                }
                Install::Working(step) => {
                    ui.label(RichText::new(step.as_str()).color(MUTED));
                }
                Install::Started => {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            if let Install::Failed(why) = &install {
                ui.label(
                    RichText::new(format!("Update failed: {why}"))
                        .small()
                        .color(theme::WARN),
                );
            }
            if link(ui, "What's new", MUTED)
                .on_hover_text("Opens the release page.")
                .clicked()
            {
                update::open(version);
            }
        });
    ui.add_space(12.0);
}

/// Reads an invite link, or says plainly what is wrong with it.
fn check_invite(text: &str) -> Result<InviteLink, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(String::from("Paste the invite link you were sent."));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    InviteLink::parse(text, now).map_err(|e| format!("This invite link does not work: {e}"))
}

pub(crate) fn start(app: &mut VeilApp, host: bool) {
    if app.name.trim().is_empty() {
        app.home_error = Some(String::from("Enter your name first."));
        return;
    }
    app.home_error = None;
    app.nearby.stop();
    app.go_pre_join(host);
}

/// After leaving: how long the call lasted, and the way back.
pub fn ended(app: &mut VeilApp, ui: &mut egui::Ui) {
    let crate::app::Screen::Ended {
        title,
        lasted,
        reason,
    } = &app.screen
    else {
        return;
    };
    let (title, lasted, reason) = (title.clone(), *lasted, reason.clone());
    page(ui, "ended", |ui| {
        ui.label(RichText::new(reason).font(theme::bold(28.0)));
        ui.label(RichText::new(format!("{title} · {}", clock(lasted))).color(MUTED));
        ui.add_space(20.0);
        if pill(ui, None, "Back to home", Tone::Primary, 220.0).clicked() {
            app.go_home();
        }
        if ui.link("Connection report").clicked() {
            app.diagnostics.open();
        }
    });
}

/// A column on the page background, centred both ways. `id` names the screen: the column's
/// height is remembered from the last frame, so a screen settles one frame after it first
/// shows or changes height.
pub fn page(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE))
        .show(ui, |ui| {
            let key = ui.make_persistent_id(("page", id));
            let last: f32 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(0.0);
            ui.add_space(top_space(ui.available_height(), last));
            let height = ui
                .with_layout(Layout::top_down(Align::Center), |ui| {
                    ui.set_max_width(COLUMN);
                    add(ui);
                })
                .response
                .rect
                .height();
            if (height - last).abs() > 0.5 {
                ui.ctx().data_mut(|d| d.insert_temp(key, height));
                ui.ctx().request_repaint();
            }
        });
}

/// Space above a column `content` high on a page `available` high: a little above centre,
/// which reads as centred, and never less than a small margin.
fn top_space(available: f32, content: f32) -> f32 {
    ((available - content) * 0.45).max(24.0)
}

#[cfg(test)]
mod tests {
    use super::top_space;
    use super::{HomeHelp, HomeHelpState, track_help};
    use eframe::egui;

    fn help_frame(
        ctx: &egui::Context,
        pointer: Option<egui::Pos2>,
        focus_name: bool,
    ) -> (HomeHelp, [egui::Rect; 2]) {
        let event = egui::Event::PointerMoved(pointer.unwrap_or(egui::pos2(500.0, 500.0)));
        let input = egui::RawInput {
            events: vec![event],
            ..Default::default()
        };
        let mut active = HomeHelp::Welcome;
        let mut rects = [egui::Rect::NOTHING; 2];
        let mut output = ctx.run_ui(input, |ui| {
            let mut help = HomeHelpState::default();
            for (index, topic) in [HomeHelp::Name, HomeHelp::Join].into_iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(200.0, 40.0), egui::Sense::click());
                rects[index] = rect;
                if index == 0 && focus_name {
                    response.request_focus();
                }
                track_help(&response, topic, &mut help);
            }
            active = help.active();
        });
        output.textures_delta.clear();
        (active, rects)
    }

    #[test]
    fn hover_help_follows_pointer_and_returns_to_about() {
        let ctx = egui::Context::default();
        let (_, rects) = help_frame(&ctx, None, false);
        assert_eq!(
            help_frame(&ctx, Some(rects[0].center()), false).0,
            HomeHelp::Name
        );
        assert_eq!(
            help_frame(&ctx, Some(rects[1].center()), false).0,
            HomeHelp::Join
        );
        assert_eq!(help_frame(&ctx, None, false).0, HomeHelp::Welcome);
    }

    #[test]
    fn hover_help_overrides_focus_then_restores_focused_help() {
        let ctx = egui::Context::default();
        let (_, rects) = help_frame(&ctx, None, true);
        assert_eq!(help_frame(&ctx, None, true).0, HomeHelp::Name);
        assert_eq!(
            help_frame(&ctx, Some(rects[1].center()), true).0,
            HomeHelp::Join
        );
        assert_eq!(help_frame(&ctx, None, true).0, HomeHelp::Name);
    }

    #[test]
    fn a_short_column_sits_just_above_centre() {
        // 800 high page, 400 high column: 200 each side if centred, a little less above.
        assert_eq!(top_space(800.0, 400.0), 180.0);
    }

    #[test]
    fn a_tall_column_keeps_a_small_margin() {
        assert_eq!(top_space(500.0, 900.0), 24.0);
    }
}
