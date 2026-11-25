//! Android layouts share the desktop call state, security panels and media engine.
mod call_bar;
mod home;
mod prejoin;
pub mod sheet;
mod waiting;
pub(crate) mod widgets;

use crate::app::{Screen, VeilApp};
use crate::icons::Icon;
use crate::model::CallStatus;
use crate::screens::{
    call::CallView,
    controls,
    panels::{self, Panel},
};
use crate::theme;
use crate::widgets::Tone;
use eframe::egui::{
    self, Align, Align2, Layout, Margin, Rect, Sense, vec2,
};
use std::time::{Duration, Instant};
use widgets as w;

pub use home::home;
pub use prejoin::prejoin;

const EDGE: i8 = 20;

#[cfg(target_os = "android")]
pub fn apply_style(ctx: &egui::Context, scale: f32) {
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 8.0);
        // Labels keep their natural height. Only interactive controls reserve touch space.
        style.spacing.interact_size = vec2(48.0, 24.0);
        style.spacing.button_padding = vec2(12.0, 8.0);
        style.spacing.combo_height = 280.0;
        for (role, size) in [
            (egui::TextStyle::Body, 16.0),
            (egui::TextStyle::Button, 14.0),
            (egui::TextStyle::Small, 13.0),
            (egui::TextStyle::Heading, 22.0),
        ] {
            style
                .text_styles
                .insert(role, egui::FontId::proportional(size * scale));
        }
        style.visuals.widgets.inactive.corner_radius = 12.into();
        style.visuals.widgets.hovered.corner_radius = 12.into();
        style.visuals.widgets.active.corner_radius = 12.into();
    });
}

pub fn page(ui: &mut egui::Ui, id: &str, draw: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(EDGE))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, draw);
        });
}

/// The same app bar is used for Home, pre-join and settings.
pub fn app_bar(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    back: bool,
    settings: bool,
) -> (bool, bool) {
    let mut result = (false, false);
    egui::Panel::top(id)
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(Margin {
            left: 12,
            right: 12,
            top: 4,
            bottom: 4,
        }))
        // No hairline under the bar: the page colour already separates it.
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if back {
                    result.0 = w::back(ui).clicked();
                } else {
                    let (rect, _) = ui.allocate_exact_size(vec2(36.0, 48.0), Sense::hover());
                    crate::logo::animated(
                        ui,
                        Rect::from_center_size(rect.center(), vec2(32.0, 32.0)),
                        theme::PAGE,
                    );
                }
                let width = (ui.available_width() - if settings { 56.0 } else { 0.0 }).max(1.0);
                ui.allocate_ui_with_layout(
                    vec2(width, 48.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(width);
                        w::label(ui, title, 22.0, true, theme::TEXT);
                    },
                );
                if settings {
                    result.1 = w::icon(ui, Icon::Signal, "Connection settings", false).clicked();
                }
            });
        });
    result
}

pub fn action(ui: &mut egui::Ui, icon: Option<Icon>, label: &str, tone: Tone) -> bool {
    w::action(ui, icon, label, tone, ui.available_width()).clicked()
}

pub fn setting(ui: &mut egui::Ui, value: &mut bool, title: &str, detail: &str) -> egui::Response {
    w::switch(ui, value, title, detail)
}

pub fn copy(ctx: &egui::Context, value: &str) {
    #[cfg(target_os = "android")]
    veil_media::ffi::android::copy_text(value);
    #[cfg(not(target_os = "android"))]
    ctx.copy_text(value.to_owned());
    let _ = ctx;
}

/// The Data use row of a menu. Tapping it sets `page`, and the caller shows
/// [`data_choices`] in the same menu, since egui cannot open a pop-up from inside one.
fn data_row(app: &VeilApp, ui: &mut egui::Ui, page: &mut bool) {
    let value = match app.data_use.get() {
        crate::data_use::DataUse::Auto => "Auto",
        crate::data_use::DataUse::Saver => "Saver",
        crate::data_use::DataUse::AudioOnly => "Audio",
    };
    if w::menu_value(ui, Some(Icon::Signal), "Data use", value).clicked() {
        *page = true;
    }
}

/// The Data use choices as menu rows, the current one checked. Closes the menu on a pick.
fn data_choices(app: &mut VeilApp, ui: &mut egui::Ui) {
    let current = app.data_use.get();
    for choice in crate::data_use::DataUse::ALL {
        if w::menu_item(ui, None, choice.label(), current == choice).clicked() {
            app.set_data_use(choice);
            ui.close();
        }
    }
}

/// The Data use choices, opened from `response`.
fn data_menu(app: &mut VeilApp, response: &egui::Response) {
    w::menu(response, |ui| data_choices(app, ui));
}

fn flip(app: &mut VeilApp) {
    app.devices = crate::devices::DeviceLists::load();
    let current = app.media.choice().camera.as_deref();
    let cameras = &app.devices.cameras;
    if !cameras.is_empty() {
        let index = current
            .and_then(|id| cameras.iter().position(|camera| camera.id == id))
            .unwrap_or(0);
        app.media
            .choose_camera(Some(cameras[(index + 1) % cameras.len()].id.clone()));
    }
}

pub fn call(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
    let mut stay = true;
    let confirm = egui::Id::unique("mobile-leave");
    if ui
        .ctx()
        .data(|data| data.get_temp::<bool>(confirm))
        .unwrap_or(false)
    {
        egui::Window::new("Leave this call?")
            .collapsible(false)
            .resizable(false)
            .default_width(280.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                if action(ui, None, "Leave call", Tone::Danger) {
                    stay = false;
                    ui.ctx().data_mut(|data| data.remove::<bool>(confirm));
                }
                if action(ui, None, "Stay in call", Tone::Quiet) {
                    ui.ctx().data_mut(|data| data.remove::<bool>(confirm));
                }
            });
    }
    egui::Panel::top("mobile-call-bar")
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(Margin {
            left: 16,
            right: 12,
            top: 8,
            bottom: 8,
        }))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 168.0).max(1.0);
                ui.allocate_ui_with_layout(vec2(width, 48.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_min_width(width);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    w::label(ui, "Veil call", 20.0, true, theme::TEXT);
                    let status = match view.call.status {
                        CallStatus::Connecting => "Connecting…",
                        CallStatus::Lobby => "Waiting for host",
                        CallStatus::Reconnecting => "Reconnecting…",
                        _ => "Encrypted",
                    };
                    w::label(ui, status, 12.0, false, theme::MUTED);
                });
                let people = w::icon(
                    ui,
                    Icon::People,
                    "People and lobby",
                    view.panel == Some(Panel::People),
                );
                w::badge(ui, &people, view.call.lobby.len());
                if people.clicked() {
                    view.panel = if view.panel == Some(Panel::People) {
                        None
                    } else {
                        Some(Panel::People)
                    };
                }
                let chat = w::icon(ui, Icon::Chat, "Chat", view.panel == Some(Panel::Chat));
                w::badge(ui, &chat, view.call.unread);
                if chat.clicked() {
                    view.panel = if view.panel == Some(Panel::Chat) {
                        None
                    } else {
                        Some(Panel::Chat)
                    };
                }
                let more = w::more(ui);
                let page_id = more.id.with("data-page");
                let mut page = ui.ctx().data(|d| d.get_temp::<bool>(page_id)).unwrap_or(false);
                let shown = w::menu(&more, |ui| {
                    if page {
                        if w::menu_back(ui, "Data use").clicked() {
                            page = false;
                        }
                        data_choices(app, ui);
                        return;
                    }
                    if view.call.is_host
                        && w::menu_item(ui, Some(Icon::Link), "Invite someone", false).clicked()
                    {
                        view.mobile_invite = !view.mobile_invite;
                        view.panel = None;
                        ui.close();
                    }
                    if w::menu_item(ui, Some(Icon::Lock), "Call security", false).clicked() {
                        view.panel = Some(Panel::Security);
                        ui.close();
                    }
                    let mirrored = app.media.mirror();
                    if w::menu_item(ui, Some(Icon::Mirror), "Mirror my camera", mirrored)
                        .on_hover_text(controls::MIRROR_HINT)
                        .clicked()
                    {
                        app.media.set_mirror(!mirrored);
                    }
                    if w::menu_item(ui, Some(Icon::Info), "Connection report", false).clicked() {
                        app.diagnostics.open();
                        ui.close();
                    }
                    data_row(app, ui, &mut page);
                });
                // A closed menu always reopens on its first page.
                let page = page && shown.is_some();
                ui.ctx().data_mut(|d| d.insert_temp(page_id, page));
            });
        });
    if !call_bar::show(view, app, ui) {
        stay = false;
    }
    let page_id = match view.panel {
        Some(Panel::People) => "mobile-people",
        Some(Panel::Chat) => "mobile-chat",
        Some(Panel::Security) => "mobile-security",
        None => "mobile-stage",
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(12))
        .show(ui, |ui| {
            if let Some(panel) = view.panel {
                ui.horizontal(|ui| {
                    if w::back(ui).clicked() {
                        view.panel = None;
                    }
                    w::label(
                        ui,
                        match panel {
                            Panel::People => "People",
                            Panel::Chat => "Chat",
                            Panel::Security => "Security",
                        },
                        22.0,
                        true,
                        theme::TEXT,
                    );
                });
                ui.add_space(8.0);
                if panel == Panel::Chat {
                    panels::mobile_content(view, ui, panel, Instant::now());
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(page_id)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            panels::mobile_content(view, ui, panel, Instant::now())
                        });
                }
                return;
            }
            if view.mobile_invite && view.call.is_host {
                let ctx = ui.ctx().clone();
                view.mobile_invite = sheet::show(&ctx, "invite-sheet", "Invite", |ui| {
                    invitation(view, ui);
                });
            }
            if !view.call.lobby.is_empty() {
                if action(
                    ui,
                    Some(Icon::People),
                    &format!("{} waiting to join", view.call.lobby.len()),
                    Tone::Primary,
                ) {
                    view.panel = Some(Panel::People);
                }
                ui.add_space(8.0);
            }
            if let Some((message, at)) = &view.notice
                && at.elapsed() < Duration::from_secs(3)
            {
                w::label(ui, message, 13.0, false, w::ON_SELECTED);
            }
            stage(view, app, ui);
        });
    if view.panel == Some(Panel::Chat) {
        view.call.unread = 0;
    }
    stay
}

fn invitation(view: &mut CallView, ui: &mut egui::Ui) {
    if view.call.invite_full.is_empty() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Preparing your invitation…");
        });
        return;
    }
    ui.vertical_centered(|ui| {
        waiting::code(view, ui);
        waiting::controls(view, ui);
    });
}

fn stage(view: &mut CallView, app: &VeilApp, ui: &mut egui::Ui) {
    let size = ui.available_size();
    let (area, _) = ui.allocate_exact_size(vec2(size.x, size.y.max(1.0)), Sense::hover());
    if view.call.participants.iter().all(|person| person.is_you) {
        waiting::show(view, app, ui, area);
    } else {
        crate::screens::bento::show(view, app, ui, area, false);
    }
}

pub fn ended(app: &mut VeilApp, ui: &mut egui::Ui) {
    app_bar(ui, "mobile-ended-bar", "Veil", false, false);
    page(ui, "mobile-ended", |ui| {
        let (title, lasted, reason) = match &app.screen {
            Screen::Ended {
                title,
                lasted,
                reason,
            } => (title.clone(), *lasted, reason.clone()),
            _ => return,
        };
        ui.add_space(40.0);
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, 64.0), Sense::hover());
        Icon::Leave.paint(ui.painter(), rect.center(), 32.0, theme::MUTED);
        ui.add_space(20.0);
        w::label(ui, "Call ended", 28.0, true, theme::TEXT);
        w::label(ui, reason, 16.0, false, theme::MUTED);
        ui.add_space(8.0);
        w::label(
            ui,
            format!("{title} · {}", crate::model::clock(lasted)),
            13.0,
            false,
            theme::MUTED,
        );
        ui.add_space(28.0);
        if action(ui, None, "Back to home", Tone::Primary) {
            app.go_home();
        }
        // A secondary choice, so a quiet link under the one button.
        if ui
            .vertical_centered(|ui| w::link(ui, None, "Connection report"))
            .inner
            .clicked()
        {
            app.diagnostics.open();
        }
    });
}
