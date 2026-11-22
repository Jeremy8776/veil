//! Settings > Connection: your own servers, a fixed port, and opening it on your router.
//!
//! Purpose: edits a copy of the saved [`Settings`]; Save checks every field and keeps them
//! only if all are good (`connection::Store::save`), showing each problem under its field.
//! Back leaves without saving. A field an environment variable replaces says so.
//!
//! Invariants:
//! - Nothing here touches the network; the settings take effect at the next call.
//! - Short labels, detail in tooltips (Veil's UI style).

use eframe::egui::{self, Align, Layout, RichText, TextEdit};

use crate::app::VeilApp;
use crate::connection::{Field, MAX_FIELD, Settings, Store};
use crate::theme::{self, FORM_CONTROL_WIDTH, FORM_WIDTH, HELP_WIDTH, MUTED, PAGE_COLUMN_GAP};
use crate::widgets::{HelpState, Tone, card, help_card, pill, toggle_row};

/// The fields being edited and the outcome of the last Save.
pub struct Form {
    draft: Settings,
    problems: Vec<(Field, String)>,
    saved: bool,
}

impl Form {
    pub fn new(store: &Store) -> Self {
        Self {
            draft: store.saved.clone(),
            problems: Vec::new(),
            saved: false,
        }
    }
}

/// The contextual help shown beside the form on a wide window.
#[derive(Clone, Copy, Default)]
enum Help {
    #[default]
    Overview,
    Field(Field),
    Router,
    Hide,
    Save,
    Back,
}

impl Help {
    fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::Overview => (
                "Your next call",
                "All settings are optional. Leave fields blank to use Veil's defaults. Save changes to use them from your next call.",
            ),
            Self::Save => (
                "Save connection settings",
                "Save checks every field and keeps the settings only if all are valid. Any problems appear beneath their fields.",
            ),
            Self::Back => (
                "Back to Home",
                "Leave Connection without saving your changes.",
            ),
            Self::Field(field) => {
                let (label, _, help) = text(field);
                (label, help)
            }
            Self::Router => ("Open a port on my router", ROUTER_HELP),
            Self::Hide => ("Hide my address", HIDE_HELP),
        }
    }
}

/// Draws the screen. Returns false when the user goes back to Home.
pub fn show(app: &mut VeilApp, ui: &mut egui::Ui, form: &mut Form) -> bool {
    let mut stay = true;
    let mut help = HelpState::default();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(32))
        .show(ui, |ui| {
            let height_key = ui.make_persistent_id("connection-content-height");
            let last_height: f32 = ui.ctx().data(|d| d.get_temp(height_key)).unwrap_or(0.0);
            let top = ((ui.available_height() - last_height) * 0.5).max(0.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(top);
                let show_help = ui.available_width() >= 900.0;
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
                            ui.label(RichText::new("Connection").font(theme::bold(28.0)));
                            ui.add_space(20.0);
                            let mut helper_rect = None;
                            if show_help {
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = PAGE_COLUMN_GAP;
                                    let settings = ui.allocate_ui_with_layout(
                                        egui::vec2(FORM_WIDTH, 0.0),
                                        Layout::top_down(Align::Center),
                                        |ui| {
                                            ui.spacing_mut().item_spacing.x = theme::GAP;
                                            settings(ui, form, FORM_WIDTH, &mut help, false)
                                        },
                                    );
                                    helper_rect = Some(
                                        ui.allocate_exact_size(
                                            egui::vec2(HELP_WIDTH, settings.inner.rect.height()),
                                            egui::Sense::hover(),
                                        )
                                        .0,
                                    );
                                });
                            } else {
                                settings(ui, form, width, &mut help, true);
                            }
                            ui.add_space(16.0);
                            let actions_width = (FORM_CONTROL_WIDTH * 2.0 + theme::GAP).min(width);
                            ui.allocate_ui_with_layout(
                                egui::vec2(actions_width, 40.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    let button_width = (actions_width - theme::GAP) / 2.0;
                                    let back = pill(ui, None, "Back", Tone::Quiet, button_width);
                                    help.track(&back, Help::Back);
                                    if back.clicked() {
                                        stay = false;
                                    }
                                    let save = pill(ui, None, "Save", Tone::Primary, button_width);
                                    help.track(&save, Help::Save);
                                    if save.clicked() {
                                        match app.connection.save(form.draft.clone()) {
                                            Ok(()) => {
                                                form.problems.clear();
                                                form.saved = true;
                                            }
                                            Err(problems) => {
                                                form.problems = problems;
                                                form.saved = false;
                                            }
                                        }
                                    }
                                },
                            );
                            if form.saved && app.connection.problem.is_none() {
                                ui.label(
                                    RichText::new("Saved. Used from your next call.")
                                        .small()
                                        .color(MUTED),
                                );
                            }
                            if let Some(problem) = &app.connection.problem {
                                ui.label(RichText::new(problem).small().color(theme::WARN));
                            }
                            if let Some(rect) = helper_rect {
                                let mut helper = ui.new_child(
                                    egui::UiBuilder::new()
                                        .id_salt("connection-helper")
                                        .max_rect(rect)
                                        .layout(Layout::top_down(Align::Center)),
                                );
                                help_card(&mut helper, rect.height(), |ui| {
                                    let (title, detail) = help.active().text();
                                    ui.label(RichText::new(title).font(theme::bold(20.0)));
                                    ui.add_space(8.0);
                                    ui.label(RichText::new(detail).color(MUTED));
                                });
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
    stay
}

fn settings(
    ui: &mut egui::Ui,
    form: &mut Form,
    width: f32,
    help: &mut HelpState<Help>,
    show_tooltips: bool,
) -> egui::Response {
    let overridden = Settings::overridden(|name| std::env::var(name).ok());
    card()
        .show(ui, |ui| {
            ui.set_width((width - 42.0).max(0.0));
            for field in Field::ALL {
                field_input(ui, form, field, &overridden, help, show_tooltips);
                ui.add_space(12.0);
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(8.0);
            let router = toggle_row(
                ui,
                &mut form.draft.open_port,
                "Open a port on my router",
                "Temporarily open the ports guests need while you host a call.",
            );
            help.track(&router, Help::Router);
            if router.changed() {
                form.draft.asked_open_port = true;
                form.saved = false;
            }
            let hide = toggle_row(
                ui,
                &mut form.draft.hide_address,
                "Hide my address",
                "Use the relay when joining, so other people cannot see your IP address.",
            );
            help.track(&hide, Help::Hide);
            if hide.changed() {
                form.saved = false;
            }
            if show_tooltips {
                router.on_hover_text(ROUTER_HELP);
                hide.on_hover_text(HIDE_HELP);
            }
        })
        .response
}

/// One labelled field, including validation and environment override feedback.
fn field_input(
    ui: &mut egui::Ui,
    form: &mut Form,
    field: Field,
    overridden: &[Field],
    state: &mut HelpState<Help>,
    show_tooltips: bool,
) {
    let (label, hint, help) = text(field);
    let label = ui.label(RichText::new(label).color(MUTED));
    let edit = ui
        .add(
            TextEdit::singleline(form.draft.field_mut(field))
                .align(egui::Align2::CENTER_CENTER)
                .hint_text(hint)
                .char_limit(MAX_FIELD)
                .desired_width(ui.available_width().min(FORM_CONTROL_WIDTH)),
        )
        .labelled_by(label.id);
    state.track(&label, Help::Field(field));
    state.track(&edit, Help::Field(field));
    if edit.changed() {
        form.saved = false;
        form.problems.retain(|(f, _)| *f != field);
    }
    if show_tooltips {
        label.on_hover_text(help);
        edit.on_hover_text(help);
    }
    if overridden.contains(&field) {
        ui.label(
            RichText::new(format!("Replaced by {} for now", field.env()))
                .small()
                .color(theme::WARN),
        )
        .on_hover_text(
            "An environment variable is set, so calls use its value instead of \
             this one. Remove the variable to use this setting.",
        );
    }
    for (_, problem) in form.problems.iter().filter(|(f, _)| *f == field) {
        ui.label(RichText::new(problem).small().color(theme::DANGER_HOVER));
    }
}

/// The router setting's tooltip, also used by the question before the first hosted call.
pub const ROUTER_HELP: &str = "While you host a call, Veil asks your router (UPnP, NAT-PMP or \
     PCP) to open the call's UDP port, and the UDP port guests use to reach each other, so \
     people on the internet can join. Two ports, for that call only: renewed while it runs, \
     removed when it ends. With your own rendezvous set, only the call's port is opened. \
     Your router must allow this; if it does not, Veil shows what to do instead.";

const HIDE_HELP: &str = "When you join a call, reach everyone through the call's relay \
     only, so the others never learn your IP address. The relay still sees it, as it sees \
     everyone's. Slightly slower, and the call fails if the relay cannot be reached. Has no \
     effect when you host: your invite carries your addresses. Off by default.";

/// Label, hint and tooltip for each field.
fn text(field: Field) -> (&'static str, &'static str, &'static str) {
    match field {
        Field::Rendezvous => (
            "Rendezvous and relay",
            "KEY@HOST:PORT",
            "A veil-node that helps guests find you, and relays when a direct link fails. It \
             sees IP addresses and timing, never your call. HOST is an IP or a DNS name; the \
             key stops any other server answering. Blank: Veil runs one on your computer for \
             each call you host.",
        ),
        Field::Forwarder => (
            "Forwarder",
            "KEY@HOST:PORT",
            "A veil-node that carries calls of 5 or more people, so your upload does not grow \
             with each person. It sees sealed frames only. HOST is an IP or a DNS name. Blank: \
             your own, which only people on your network can reach.",
        ),
        Field::Advertise => (
            "Public addresses",
            "IP:PORT, IP:PORT",
            "Addresses guests can dial you on directly, such as your router's public IP with \
             a port forwarded to this computer. Veil puts them first in every invite.",
        ),
        Field::Port => (
            "Call port",
            "Any",
            "A fixed UDP port for the calls you host. Needed if you forward a port on your \
             router. Blank: any free port.",
        ),
    }
}
