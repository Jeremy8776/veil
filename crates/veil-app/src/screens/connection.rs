//! Settings > Connection: your own servers and a fixed port.
//!
//! Purpose: edits a copy of the saved [`Settings`]; Save checks every field and keeps them
//! only if all are good (`connection::Store::save`), showing each problem under its field.
//! Back leaves without saving. A field an environment variable replaces says so.
//!
//! Invariants:
//! - Nothing here touches the network; the settings take effect at the next call.
//! - Short labels, detail in tooltips (Veil's UI style).

use eframe::egui::{self, RichText, TextEdit};

use crate::app::VeilApp;
use crate::connection::{Field, MAX_FIELD, Settings, Store};
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, card, pill};

const WIDTH: f32 = 400.0;

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

/// Draws the screen. Returns false when the user goes back to Home.
pub fn show(app: &mut VeilApp, ui: &mut egui::Ui, form: &mut Form) -> bool {
    let overridden = Settings::overridden(|name| std::env::var(name).ok());
    let mut stay = true;
    super::home::page(ui, |ui| {
        ui.label(RichText::new("Connection").font(theme::bold(28.0)));
        ui.label(RichText::new("Optional. Blank uses Veil's own.").color(MUTED));
        ui.add_space(16.0);
        card().show(ui, |ui| {
            ui.set_width(WIDTH);
            for field in Field::ALL {
                let (label, hint, help) = text(field);
                ui.label(RichText::new(label).color(MUTED))
                    .on_hover_text(help);
                let edit = ui.add(
                    TextEdit::singleline(form.draft.field_mut(field))
                        .hint_text(hint)
                        .char_limit(MAX_FIELD)
                        .desired_width(f32::INFINITY),
                );
                if edit.on_hover_text(help).changed() {
                    form.saved = false;
                    form.problems.retain(|(f, _)| *f != field);
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
                ui.add_space(8.0);
            }
            ui.horizontal(|ui| {
                let half = (WIDTH - theme::GAP) / 2.0;
                if pill(ui, None, "Save", Tone::Primary, half).clicked() {
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
                if pill(ui, None, "Back", Tone::Quiet, half).clicked() {
                    stay = false;
                }
            });
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
        });
    });
    stay
}

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
