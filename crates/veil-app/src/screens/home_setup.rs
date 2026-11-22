//! A persistent summary of saved intent; only explicit checks prove server reachability.

use super::home::{HomeHelp, HomeHelpState, helper_text, track_help};
use crate::connection::{Field, Store};
use crate::theme::{self, MUTED};
use eframe::egui::{self, Align, Layout, RichText};

pub(super) enum Action {
    Activate(u64),
    Manage,
}

pub(super) fn helper_card(
    ui: &mut egui::Ui,
    help: &mut HomeHelpState,
    height: f32,
    store: &Store,
) -> Option<Action> {
    let mut action = None;
    crate::widgets::card().show(ui, |ui| {
        ui.set_width(theme::HELP_WIDTH - 42.0);
        ui.set_min_height((height - 42.0).max(0.0));
        // Reserve the upper text, then draw it after the summary has reported hover/focus.
        let text_rect = ui
            .scope_builder(egui::UiBuilder::new().invisible(), |ui| {
                helper_text(ui, help.active());
            })
            .response
            .rect;
        let gap = (ui.max_rect().bottom() - ui.cursor().top() - 204.0).max(8.0);
        ui.add_space(gap);
        ui.separator();
        ui.add_space(4.0);
        action = summary(ui, store, help, false);
        let mut text_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("setup-context-help")
                .max_rect(text_rect)
                .layout(Layout::top_down(Align::Center)),
        );
        helper_text(&mut text_ui, help.active());
    });
    action
}

pub(super) fn summary(
    ui: &mut egui::Ui,
    store: &Store,
    help: &mut HomeHelpState,
    show_tooltips: bool,
) -> Option<Action> {
    let mut action = None;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(RichText::new("Your setup").font(theme::bold(16.0)));
        let mut selected = store.active_id();
        let selector = egui::ComboBox::from_id_salt("home-active-setup")
            .truncate()
            .width(ui.available_width().min(theme::FORM_CONTROL_WIDTH))
            .selected_text(store.active_name())
            .show_ui(ui, |ui| {
                for setup in store.setups() {
                    let option = ui.selectable_value(&mut selected, setup.id, &setup.name);
                    track_help(&option, HomeHelp::SetupSelector, help);
                }
            })
            .response;
        track_help(&selector, HomeHelp::SetupSelector, help);
        if show_tooltips {
            selector.on_hover_text(HomeHelp::SetupSelector.text().1);
        }
        if selected != store.active_id() {
            action = Some(Action::Activate(selected));
        }
        ui.add_space(4.0);
        match store.summary_config() {
            Ok(config) => {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    setup_row(
                        ui,
                        help,
                        "Calls",
                        if config.local_only {
                            "Local only"
                        } else {
                            "Internet + local"
                        },
                        theme::TEXT,
                        HomeHelp::SetupCalls(config.local_only),
                        show_tooltips,
                    );
                    setup_row(
                        ui,
                        help,
                        "Joining",
                        if config.relay_only {
                            "Address hidden"
                        } else {
                            "Direct allowed"
                        },
                        theme::TEXT,
                        HomeHelp::SetupJoining(config.relay_only),
                        show_tooltips,
                    );
                    let (servers, colour) = server_status(store, &config);
                    setup_row(
                        ui,
                        help,
                        "Servers",
                        servers,
                        colour,
                        HomeHelp::SetupServers,
                        show_tooltips,
                    );
                    setup_row(
                        ui,
                        help,
                        "Hosting",
                        "Not verified",
                        theme::TEXT,
                        HomeHelp::SetupHosting(config.local_only),
                        show_tooltips,
                    );
                });
            }
            Err(problem) => {
                ui.label(RichText::new(problem).small().color(theme::WARN));
            }
        }
        let overrides = Field::ALL
            .iter()
            .any(|field| std::env::var(field.env()).is_ok_and(|value| !value.trim().is_empty()));
        let manage_label = if store.problem.is_some() {
            "Review connection settings"
        } else if overrides {
            "Manage · environment overrides"
        } else {
            "Manage connections"
        };
        ui.add_space(4.0);
        let manage = ui.add(egui::Link::new(
            RichText::new(manage_label)
                .color(theme::ACCENT_HOVER)
                .underline(),
        ));
        track_help(&manage, HomeHelp::SetupManage, help);
        let manage = if show_tooltips {
            manage.on_hover_text(
                store
                    .problem
                    .as_deref()
                    .unwrap_or(HomeHelp::SetupManage.text().1),
            )
        } else {
            manage
        };
        if manage.clicked() {
            action = Some(Action::Manage);
        }
    });
    action
}

/// A stable label/value list with centered text inside each column.
fn status_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    colour: egui::Color32,
) -> egui::Response {
    let width = ui.available_width().min(theme::FORM_CONTROL_WIDTH);
    let row = ui
        .allocate_ui_with_layout(
            egui::vec2(width, 18.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(72.0, 18.0),
                    Layout::top_down(Align::Center),
                    |ui| {
                        ui.label(RichText::new(label).small().color(MUTED));
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(width - 80.0, 18.0),
                    Layout::top_down(Align::Center),
                    |ui| {
                        ui.label(RichText::new(value).font(theme::bold(14.0)).color(colour));
                    },
                );
            },
        )
        .response;
    let response = ui.interact(
        row.rect,
        egui::Id::unique(("home-setup-status", label)),
        egui::Sense::focusable_noninteractive(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Label,
            ui.is_enabled(),
            format!("{label}: {value}"),
        )
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            row.rect.expand(2.0),
            4,
            egui::Stroke::new(1.0, theme::TEXT),
            egui::StrokeKind::Outside,
        );
    }
    response
}

fn setup_row(
    ui: &mut egui::Ui,
    help: &mut HomeHelpState,
    label: &str,
    value: &str,
    colour: egui::Color32,
    topic: HomeHelp,
    show_tooltips: bool,
) {
    let response = status_row(ui, label, value, colour);
    track_help(&response, topic, help);
    if show_tooltips {
        response.on_hover_text(topic.text().1);
    }
}

fn server_status(
    store: &Store,
    config: &crate::engine::plan::NetConfig,
) -> (&'static str, egui::Color32) {
    if config.local_only {
        return ("Off", MUTED);
    }
    if config.rendezvous.is_none() && config.forwarder.is_none() {
        return ("Built in", theme::TEXT);
    }
    let checked = store.checked.as_ref().filter(|checked| {
        checked.config == *config && checked.at.elapsed() < std::time::Duration::from_secs(300)
    });
    match checked.and_then(|checked| checked.servers_reachable) {
        Some(true) => ("Reachable", theme::SPEAKING),
        Some(false) => ("Check failed", theme::WARN),
        None => ("Not checked", theme::TEXT),
    }
}

pub(super) fn apply(app: &mut crate::app::VeilApp, action: Action) {
    match action {
        Action::Activate(id) => match app.connection.activate(id) {
            Ok(()) => {
                app.nearby.stop();
                app.updates
                    .set_network_allowed(!app.connection.saved.local_only);
                app.data_use.set(app.connection.saved.data_use);
                app.home_error = app.connection.problem.clone();
            }
            Err(problem) => {
                app.home_error = Some(format!("Cannot use this setup: {problem}"));
            }
        },
        Action::Manage => {
            app.nearby.stop();
            app.screen = crate::app::Screen::Connection(Box::new(super::connection::Form::new(
                &app.connection,
            )));
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn help_frame(
        ctx: &egui::Context,
        pointer: egui::Pos2,
    ) -> (HomeHelp, Vec<egui::epaint::ClippedShape>) {
        let mut active = HomeHelp::Welcome;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            },
            |ui| {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(24.0, 24.0),
                    egui::vec2(theme::HELP_WIDTH, 396.0),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                let mut help = HomeHelpState::default();
                helper_card(&mut child, &mut help, 396.0, &Store::open(None));
                active = help.active();
            },
        );
        output.textures_delta.clear();
        (active, output.shapes)
    }

    #[cfg(windows)]
    #[test]
    fn summary_hover_and_keyboard_focus_update_the_visible_helper_in_the_same_frame() {
        let ctx = egui::Context::default();
        theme::load_fonts(&ctx).expect("Windows fonts");
        theme::apply(&ctx);
        let away = egui::pos2(900.0, 700.0);
        help_frame(&ctx, away);
        for (label, expected) in [
            ("Calls", HomeHelp::SetupCalls(false)),
            ("Joining", HomeHelp::SetupJoining(false)),
            ("Servers", HomeHelp::SetupServers),
            ("Hosting", HomeHelp::SetupHosting(false)),
        ] {
            let rect = ctx
                .read_response(egui::Id::unique(("home-setup-status", label)))
                .expect("status row")
                .rect;
            let (active, shapes) = help_frame(&ctx, rect.center());
            assert_eq!(active, expected);
            assert!(shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == expected.text().0)), "helper text was not painted for {expected:?}");
        }
        assert_eq!(help_frame(&ctx, away).0, HomeHelp::Welcome);
        ctx.memory_mut(|memory| {
            memory.request_focus(egui::Id::unique(("home-setup-status", "Hosting")))
        });
        assert_eq!(help_frame(&ctx, away).0, HomeHelp::SetupHosting(false));
    }

    #[test]
    fn reachability_requires_a_recent_check_of_the_same_effective_config() {
        let mut store = Store::open(None);
        let config = crate::engine::plan::NetConfig {
            rendezvous: Some(
                veil_net::NodeAddr {
                    key: veil_security::Identity::generate().public(),
                    addr: "127.0.0.1:4433".parse().expect("address"),
                }
                .into(),
            ),
            ..Default::default()
        };
        assert_eq!(server_status(&store, &config).0, "Not checked");
        store.checked = Some(crate::connection::Checked {
            config: config.clone(),
            at: std::time::Instant::now(),
            servers_reachable: Some(true),
        });
        assert_eq!(server_status(&store, &config).0, "Reachable");
        let changed = crate::engine::plan::NetConfig {
            port: 7710,
            ..config.clone()
        };
        assert_eq!(server_status(&store, &changed).0, "Not checked");
        store.checked.as_mut().expect("report").at -= std::time::Duration::from_secs(301);
        assert_eq!(server_status(&store, &config).0, "Not checked");
    }

    #[cfg(windows)]
    #[test]
    fn every_home_helper_topic_fits_with_the_persistent_setup_summary() {
        let ctx = egui::Context::default();
        theme::load_fonts(&ctx).expect("Windows fonts");
        theme::apply(&ctx);
        for topic in [
            HomeHelp::Welcome,
            HomeHelp::Name,
            HomeHelp::NewCall,
            HomeHelp::Join,
            HomeHelp::Demo,
            HomeHelp::Connection,
            HomeHelp::DeviceKey,
            HomeHelp::NewKey,
            HomeHelp::Logs,
            HomeHelp::Updates,
            HomeHelp::SetupSelector,
            HomeHelp::SetupCalls(false),
            HomeHelp::SetupCalls(true),
            HomeHelp::SetupJoining(false),
            HomeHelp::SetupJoining(true),
            HomeHelp::SetupServers,
            HomeHelp::SetupHosting(false),
            HomeHelp::SetupHosting(true),
            HomeHelp::SetupManage,
        ] {
            let store = Store::open(None);
            let height = 396.0;
            let mut measured = 0.0;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(24.0, 24.0),
                    egui::vec2(theme::HELP_WIDTH, height),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::Center)),
                );
                let (_, focused) = ui.allocate_exact_size(
                    egui::vec2(0.0, 0.0),
                    egui::Sense::focusable_noninteractive(),
                );
                focused.request_focus();
                let mut help = HomeHelpState::default();
                track_help(&focused, topic, &mut help);
                helper_card(&mut child, &mut help, height, &store);
                measured = child.min_rect().height();
            });
            output.textures_delta.clear();
            assert!(
                measured <= height + 1.0,
                "helper overflow: {topic:?}, {measured}"
            );
        }
    }
}
