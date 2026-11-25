//! Android layouts share the desktop call state, security panels and media engine.
mod call_bar;
mod home;
mod prejoin;
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
    self, Align, Align2, Layout, Margin, Rect, Sense, UiBuilder, vec2,
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

fn data_picker(app: &mut VeilApp, ui: &mut egui::Ui) {
    let current = app.data_use.get();
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 148.0).max(1.0);
        ui.allocate_ui_with_layout(
            vec2(width, 48.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(width);
                w::label(ui, "Data use", 16.0, false, theme::TEXT);
            },
        );
        let label = match current {
            crate::data_use::DataUse::Auto => "Auto",
            crate::data_use::DataUse::Saver => "Data saver",
            crate::data_use::DataUse::AudioOnly => "Audio only",
        };
        let response = w::action(ui, None, label, Tone::Quiet, 140.0);
        data_menu(app, &response);
    });
}

/// The Data use choices, opened from `response`.
fn data_menu(app: &mut VeilApp, response: &egui::Response) {
    let current = app.data_use.get();
    egui::Popup::menu(response).show(|ui| {
        ui.set_min_width(240.0);
        for choice in crate::data_use::DataUse::ALL {
            if ui
                .add_sized(
                    [ui.available_width(), 48.0],
                    egui::Button::new(choice.label()).selected(current == choice),
                )
                .clicked()
            {
                app.set_data_use(choice);
                ui.close();
            }
        }
    });
}

fn mirror_choice(app: &VeilApp, ui: &mut egui::Ui) {
    let mut mirrored = app.media.mirror();
    if w::switch(ui, &mut mirrored, "Mirror my camera", controls::MIRROR_HINT).changed() {
        app.media.set_mirror(mirrored);
    }
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
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(240.0);
                    if view.call.is_host
                        && action(ui, Some(Icon::Link), "Invite someone", Tone::Quiet)
                    {
                        view.mobile_invite = !view.mobile_invite;
                        view.panel = None;
                        ui.close();
                    }
                    if action(ui, Some(Icon::Lock), "Call security", Tone::Quiet) {
                        view.panel = Some(Panel::Security);
                        ui.close();
                    }
                    mirror_choice(app, ui);
                    if action(ui, Some(Icon::Info), "Connection report", Tone::Quiet) {
                        app.diagnostics.open();
                        ui.close();
                    }
                    data_picker(app, ui);
                });
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
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .corner_radius(16)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - 56.0).max(1.0);
                            ui.allocate_ui_with_layout(
                                vec2(width, 48.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_min_width(width);
                                    w::label(ui, "Invite someone", 20.0, true, theme::TEXT);
                                },
                            );
                            if w::icon(ui, Icon::Close, "Close invitation", false).clicked() {
                                view.mobile_invite = false;
                            }
                        });
                        invitation(view, ui);
                    });
                ui.add_space(12.0);
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
    if !view.call.nearby_code.is_empty() {
        w::label(ui, "Nearby code", 13.0, false, theme::MUTED);
        w::label(ui, &view.call.nearby_code, 28.0, true, theme::TEXT);
        ui.add_space(4.0);
    }
    ui.columns(2, |columns| {
        if action(
            &mut columns[0],
            Some(Icon::Copy),
            "Copy invite",
            Tone::Quiet,
        ) {
            copy(columns[0].ctx(), &view.call.invite_full);
            view.notify("Invite copied");
        }
        if action(&mut columns[1], None, "Show QR", Tone::Quiet) {
            crate::invite_qr::open(columns[1].ctx());
        }
    });
    w::label(
        ui,
        "The host still decides who may join.",
        13.0,
        false,
        theme::MUTED,
    );
}

fn stage(view: &mut CallView, app: &VeilApp, ui: &mut egui::Ui) {
    let size = ui.available_size();
    let (area, _) = ui.allocate_exact_size(vec2(size.x, size.y.max(1.0)), Sense::hover());
    let waiting = view.call.participants.iter().all(|person| person.is_you);
    if let Some(rect) = crate::screens::bento::show(view, app, ui, area, waiting) {
        let content = rect.shrink(16.0);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("mobile-waiting")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let title = if view.call.is_host {
                            "Your call is ready"
                        } else if view.call.status == CallStatus::Lobby {
                            "Waiting for the host"
                        } else {
                            "Reaching the host"
                        };
                        w::label(ui, title, 22.0, true, theme::TEXT);
                        ui.add_space(8.0);
                        if view.call.is_host && !view.mobile_invite {
                            invitation(view, ui);
                        } else if !view.call.is_host {
                            ui.spinner();
                            w::label(
                                ui,
                                if view.call.status == CallStatus::Lobby {
                                    "The host will decide when to let you in."
                                } else {
                                    "Finding a connection to your call…"
                                },
                                14.0,
                                false,
                                theme::MUTED,
                            );
                        }
                    });
            },
        );
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
        if action(ui, Some(Icon::Info), "Connection report", Tone::Quiet) {
            app.diagnostics.open();
        }
    });
}
