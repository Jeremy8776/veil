//! Settings > Connection: your own servers, a fixed port, and opening it on your router.
//!
//! Purpose: edits a copy of the saved [`Settings`]; Save checks every field and keeps them
//! only if all are good (`connection::Store::save`), showing each problem under its field.
//! Back leaves without saving. A field an environment variable replaces says so.
//!
//! Invariants:
//! - Only an explicit connection check touches the network; settings apply to the next call.
//! - Short labels, detail in the contextual helper (tooltips on narrow windows).

use eframe::egui::{self, Align, Layout, RichText, TextEdit};

use crate::app::VeilApp;
use crate::connection::{Field, MAX_FIELD, SaveError, Settings, Store};
use crate::theme::{self, FORM_CONTROL_WIDTH, HELP_WIDTH, MUTED, PAGE_COLUMN_GAP};
use crate::widgets::{HelpState, Tone, card, pill, toggle_row};

mod check;
mod setups;

const CONNECTION_FORM_WIDTH: f32 = 560.0;

/// The fields being edited and the outcome of the last Save.
pub struct Form {
    draft: Settings,
    problems: Vec<(Field, String)>,
    saved: bool,
    check: check::Checker,
    results_open: bool,
    baseline: Settings,
    save_error: Option<String>,
    setup_id: Option<u64>,
    active_baseline: u64,
    setup_name: String,
    baseline_name: String,
    remove_confirm: bool,
}

impl Form {
    pub fn new(store: &Store) -> Self {
        Self {
            draft: store.saved.clone(),
            problems: Vec::new(),
            saved: false,
            check: check::Checker::default(),
            results_open: false,
            baseline: store.saved.clone(),
            save_error: None,
            setup_id: Some(store.active_id()),
            active_baseline: store.active_id(),
            setup_name: store.active_name().to_owned(),
            baseline_name: store.active_name().to_owned(),
            remove_confirm: false,
        }
    }
    fn dirty(&self) -> bool {
        self.edited() || self.setup_id != Some(self.active_baseline)
    }

    fn edited(&self) -> bool {
        self.draft != self.baseline
            || self.setup_name != self.baseline_name
            || self.setup_id.is_none()
    }

    fn load_setup(&mut self, store: &Store, id: u64) {
        if let Some(setup) = store.setups().iter().find(|s| s.id == id) {
            self.draft = setup.settings.clone();
            self.baseline = self.draft.clone();
            self.setup_name = setup.name.clone();
            self.baseline_name = setup.name.clone();
            self.setup_id = Some(id);
            self.problems.clear();
            self.save_error = None;
            self.saved = false;
            self.remove_confirm = false;
            self.check.clear();
        }
    }

    fn save(&mut self, store: &mut Store) -> bool {
        match store.save_setup(self.setup_id, &self.setup_name, self.draft.clone()) {
            Ok(()) => {
                self.baseline = store.saved.clone();
                self.setup_id = Some(store.active_id());
                self.active_baseline = store.active_id();
                self.setup_name = store.active_name().to_owned();
                self.baseline_name = self.setup_name.clone();
                self.problems.clear();
                self.save_error = None;
                self.saved = true;
                self.remove_confirm = false;
                true
            }
            Err(SaveError::Fields(problems)) => {
                self.problems = problems;
                self.save_error = Some(String::from(
                    "Changes not saved. Fix the highlighted fields.",
                ));
                self.saved = false;
                false
            }
            Err(SaveError::Conflict(problem)) => {
                self.save_error = Some(format!("Changes not saved: {problem}"));
                self.saved = false;
                false
            }
            Err(SaveError::Setup(problem)) => {
                self.save_error = Some(problem);
                self.saved = false;
                false
            }
        }
    }
}

/// The contextual help shown beside the form on a wide window.
#[derive(Clone, Copy, Debug, Default)]
enum Help {
    #[default]
    Overview,
    Field(Field),
    LocalOnly,
    Nearby,
    Router,
    Hide,
    Save,
    Back,
    Cancel,
    Setup,
}

impl Help {
    fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::Setup => (
                "Saved connections",
                "Keep a named setup for each network or VPS. Load one to edit it; Save changes makes it active for your next call. New setup copies the current fields. Discard edits before loading another setup.",
            ),
            Self::Overview => (
                "Your next call",
                "All settings are optional. Leave fields blank to use Veil's defaults. Save changes to use them from your next call.",
            ),
            Self::Save => (
                "Save connection settings",
                "Save validates the fields and checks that the connection choices can work together. Saved changes apply to your next call.",
            ),
            Self::Back => (
                "Back to Home",
                "Return Home. Your connection settings are unchanged.",
            ),
            Self::Cancel => (
                "Cancel changes",
                "Discard this draft and return Home. Your saved settings stay unchanged.",
            ),
            Self::Field(field) => {
                let (label, _, _) = text(field);
                (
                    label,
                    match field {
                        Field::Rendezvous => {
                            "Helps guests find you and relays encrypted traffic when direct links fail. Use KEY@HOST:PORT from veil-node. Leave blank to run this on your computer for calls you host."
                        }
                        Field::Forwarder => {
                            "Carries larger calls without increasing your upload for every participant. Use KEY@HOST:PORT from veil-node. Leave blank to use your own, which participants must be able to reach."
                        }
                        Field::Advertise => {
                            "Public IP:PORT addresses guests can dial directly. Separate multiple addresses with commas. They appear first in your invitation."
                        }
                        Field::Port => {
                            "A fixed UDP port for hosting. Use 1 to 65535, or leave blank to choose a free port. Needed for manual router port forwarding."
                        }
                    },
                )
            }
            Self::LocalOnly => (
                "Local only",
                "Keeps calls on connected local networks. Internet helpers, update checks and router port opening are disabled. Use a full invitation. Restart Veil for a clean local-only session; a request already in flight may finish.",
            ),
            Self::Nearby => ("Discoverable nearby", NEARBY_HELP),
            Self::Router => (
                "Open a port on my router",
                "Temporarily opens the ports guests need while you host. Ports close when the call ends. Your router must support this. Local only ignores router port opening.",
            ),
            Self::Hide => (
                "Hide my address",
                "When joining, use the relay so other participants cannot see your IP. The relay still sees it. Requires a reachable relay, is slower, and does not hide a host's address. Incompatible with Local only.",
            ),
        }
    }
}

/// Draws the screen. Returns false when the user goes back to Home.
pub fn show(app: &mut VeilApp, ui: &mut egui::Ui, form: &mut Form) -> bool {
    form.check
        .refresh(&form.draft, app.connection.saved.local_only);
    if let Some(snapshot) = form.check.snapshot() {
        app.diagnostics.check = Some(snapshot);
    }
    if cfg!(target_os = "android") {
        return mobile_show(app, ui, form);
    }
    let mut stay = true;
    let mut help = HelpState::default();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::PAGE).inner_margin(24))
        .show(ui, |ui| {
            let height_key = ui.make_persistent_id("connection-content-height");
            let last_height: f32 = ui.ctx().data(|d| d.get_temp(height_key)).unwrap_or(0.0);
            let top = ((ui.available_height() - last_height) * 0.5).max(0.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(top);
                let show_help =
                    ui.available_width() >= CONNECTION_FORM_WIDTH + PAGE_COLUMN_GAP + HELP_WIDTH;
                let width = ui.available_width().min(if show_help {
                    CONNECTION_FORM_WIDTH + PAGE_COLUMN_GAP + HELP_WIDTH
                } else {
                    CONNECTION_FORM_WIDTH
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
                                        egui::vec2(CONNECTION_FORM_WIDTH, 0.0),
                                        Layout::top_down(Align::Center),
                                        |ui| {
                                            ui.spacing_mut().item_spacing.x = theme::GAP;
                                            settings(
                                                ui,
                                                form,
                                                CONNECTION_FORM_WIDTH,
                                                &mut help,
                                                false,
                                                &mut app.connection,
                                            )
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
                                settings(ui, form, width, &mut help, true, &mut app.connection);
                            }
                            form.check
                                .refresh(&form.draft, app.connection.saved.local_only);
                            if !show_help {
                                ui.add_space(8.0);
                                egui::CollapsingHeader::new("Connection check").show(ui, |ui| {
                                    check_controls(ui, form, app.connection.saved.local_only);
                                });
                            }
                            ui.add_space(16.0);
                            let actions_width = (FORM_CONTROL_WIDTH * 2.0 + theme::GAP).min(width);
                            ui.allocate_ui_with_layout(
                                egui::vec2(actions_width, 40.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    let button_width = (actions_width - theme::GAP) / 2.0;
                                    let dirty = form.dirty();
                                    let back = pill(
                                        ui,
                                        None,
                                        if dirty { "Cancel" } else { "Back" },
                                        Tone::Quiet,
                                        button_width,
                                    );
                                    help.track(
                                        &back,
                                        if dirty { Help::Cancel } else { Help::Back },
                                    );
                                    if !show_help {
                                        back.clone().on_hover_text(
                                            if dirty { Help::Cancel } else { Help::Back }.text().1,
                                        );
                                    }
                                    if back.clicked() {
                                        stay = false;
                                    }
                                    let can_save = dirty || app.connection.problem.is_some();
                                    let save = ui
                                        .add_enabled_ui(can_save, |ui| {
                                            pill(
                                                ui,
                                                None,
                                                "Save changes",
                                                Tone::Primary,
                                                button_width,
                                            )
                                        })
                                        .inner;
                                    help.track(&save, Help::Save);
                                    if !show_help {
                                        save.clone()
                                            .on_hover_text(Help::Save.text().1)
                                            .on_disabled_hover_text(Help::Save.text().1);
                                    }
                                    if save.clicked() && form.save(&mut app.connection) {
                                        app.updates.set_network_allowed(!form.draft.local_only);
                                        app.data_use.set(app.connection.saved.data_use);
                                    }
                                },
                            );
                            if let Some(problem) = &form.save_error {
                                ui.label(RichText::new(problem).small().color(theme::DANGER_HOVER));
                            } else if form.dirty() {
                                ui.label(RichText::new("Unsaved changes").small().color(MUTED));
                            }
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
                                connection_helper(
                                    &mut helper,
                                    form,
                                    help.active(),
                                    rect.height(),
                                    app.connection.saved.local_only,
                                );
                            }
                            if let Some(report) = form.check.report()
                                && app
                                    .connection
                                    .summary_config()
                                    .is_ok_and(|config| report.config == config)
                            {
                                app.connection.checked = Some(report);
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
    if form.results_open && !app.diagnostics.open {
        let ctx = ui.ctx().clone();
        let width = super::dialog::width(ctx.content_rect().width(), 420.0);
        let mut report = false;
        let rows = &form.check.result;
        form.results_open = super::dialog::show(&ctx, "check-results", "Check results", width, |ui| {
            if let Some(rows) = rows {
                for row in rows {
                    let colour = match row.outcome {
                        check::Outcome::Passed => theme::SPEAKING,
                        check::Outcome::Failed => theme::DANGER_HOVER,
                        check::Outcome::Note => MUTED,
                    };
                    ui.label(RichText::new(row.label).font(theme::bold(14.0)));
                    ui.label(RichText::new(&row.detail).color(colour));
                    ui.add_space(4.0);
                }
            } else {
                ui.label(RichText::new("No results yet").color(MUTED));
            }
            let icon = Some(crate::icons::Icon::Info);
            report = pill(ui, icon, "Connection report", Tone::Quiet, width)
                .on_hover_text("Review this check's evidence before copying a redacted report.")
                .clicked();
        });
        if report {
            app.diagnostics.open_check();
        }
    }
    stay
}

fn mobile_show(app: &mut VeilApp, ui: &mut egui::Ui, form: &mut Form) -> bool {
    use crate::mobile::{self, widgets as w};
    let (back, _) = mobile::app_bar(ui, "mobile-connection-bar", "Connection", true, false);
    if back {
        app.go_home();
        return false;
    }
    egui::Panel::bottom("mobile-save-settings")
        .frame(
            egui::Frame::new()
                .fill(theme::PAGE)
                .inner_margin(egui::Margin::symmetric(20, 12)),
        )
        .show(ui, |ui| {
            if let Some(error) = &form.save_error {
                w::label(ui, error, 13.0, false, theme::WARN);
            }
            if form.saved && !form.dirty() {
                w::label(ui, "Saved for your next call", 13.0, false, theme::SPEAKING);
            }
            if ui
                .add_enabled_ui(form.dirty(), |ui| {
                    mobile::action(ui, None, "Save changes", Tone::Primary)
                })
                .inner
            {
                form.save(&mut app.connection);
            }
        });
    mobile::page(ui, "mobile-connection", |ui| {
        w::label(
            ui,
            "Choose how your next call connects.",
            16.0,
            false,
            MUTED,
        );
        if mobile::action(
            ui,
            None,
            if form.check.running() {
                "Checking…"
            } else {
                "Test these settings"
            },
            Tone::Quiet,
        ) && !form.check.running()
            && let Err(problems) = form.check.start(
                &form.draft,
                app.connection.saved.local_only,
                ui.ctx().clone(),
            ) {
            form.problems = problems;
        }
        if form.check.result.is_some()
            && mobile::action(ui, None, "Preview connection report", Tone::Quiet)
        {
            app.diagnostics.open_check();
        }
        ui.add_space(20.0);
        w::label(ui, "Local network", 20.0, true, theme::TEXT);
        mobile::setting(
            ui,
            &mut form.draft.local_only,
            "Local only",
            "Use the same Wi-Fi or hotspot. No internet helpers.",
        );
        ui.add_enabled_ui(form.draft.local_only, |ui| {
            mobile::setting(
                ui,
                &mut form.draft.nearby_discovery,
                "Nearby discovery",
                "Let others find your call while you host.",
            );
        });
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);
        w::label(ui, "Privacy", 20.0, true, theme::TEXT);
        mobile::setting(
            ui,
            &mut form.draft.hide_address,
            "Hide my address",
            "Use a relay when joining. Turn off Local only first.",
        );
        ui.add_space(16.0);
        let id = ui.scope_id().with("mobile-advanced-connection");
        let mut expanded = ui
            .ctx()
            .data(|data| data.get_temp::<bool>(id))
            .unwrap_or(false);
        if mobile::action(
            ui,
            None,
            if expanded {
                "Hide servers and ports"
            } else {
                "Servers and ports"
            },
            Tone::Quiet,
        ) {
            expanded = !expanded;
            ui.ctx().data_mut(|data| data.insert_temp(id, expanded));
        }
        // Always reveal invalid fields so a save error can be repaired.
        if expanded || !form.problems.is_empty() {
            ui.add_space(8.0);
            w::label(
                ui,
                "Optional. Leave blank to use the defaults.",
                13.0,
                false,
                MUTED,
            );
            for (field, label) in [
                (Field::Rendezvous, "Rendezvous / relay"),
                (Field::Forwarder, "Forwarder"),
                (Field::Advertise, "Advertised addresses"),
                (Field::Port, "Call port"),
            ] {
                w::label(ui, label, 13.0, true, MUTED);
                ui.add_enabled_ui(!form.draft.local_only, |ui| {
                    ui.add_sized(
                        [ui.available_width(), 48.0],
                        TextEdit::singleline(form.draft.field_mut(field)).char_limit(MAX_FIELD),
                    );
                });
                for (_, why) in form.problems.iter().filter(|(f, _)| *f == field) {
                    w::label(ui, why, 13.0, false, theme::WARN);
                }
                ui.add_space(8.0);
            }
            ui.add_enabled_ui(!form.draft.local_only, |ui| {
                mobile::setting(
                    ui,
                    &mut form.draft.open_port,
                    "Open hosting ports",
                    "Ask your router to make this device reachable.",
                );
            });
        }
    });
    true
}

fn connection_helper(
    ui: &mut egui::Ui,
    form: &mut Form,
    help: Help,
    height: f32,
    saved_local: bool,
) -> egui::Response {
    card()
        .show(ui, |ui| {
            ui.set_width(HELP_WIDTH - 42.0);
            ui.set_min_height((height - 42.0).max(0.0));
            let (title, detail) = help.text();
            ui.label(RichText::new(title).font(theme::bold(20.0)));
            ui.add_space(8.0);
            ui.label(RichText::new(detail).color(MUTED));
            // Leave a deliberate gap between contextual guidance and the check tool.
            let check_height = if form.check.result.is_some() {
                210.0
            } else {
                166.0
            };
            let remaining =
                (ui.max_rect().bottom() - ui.cursor().top() - check_height - 28.0).max(8.0);
            ui.add_space(remaining);
            ui.separator();
            ui.add_space(8.0);
            check_controls(ui, form, saved_local);
        })
        .response
}

fn check_controls(ui: &mut egui::Ui, form: &mut Form, saved_local: bool) {
    ui.label(RichText::new("Connection check").font(theme::bold(16.0)));
    ui.add_space(8.0);
    let running = form.check.running();
    let label = if running {
        "Cancel check"
    } else {
        "Test these settings"
    };
    let save_mode_first = saved_local && !form.draft.local_only;
    let scope = if save_mode_first {
        "Save changes first to allow internet tests."
    } else if form.dirty() {
        "Tests this draft without saving it."
    } else {
        "Tests your saved settings."
    };
    ui.label(
        RichText::new(scope)
            .small()
            .color(if save_mode_first { theme::WARN } else { MUTED }),
    );
    let button = ui
        .add_enabled_ui(!save_mode_first || running, |ui| {
            pill(
                ui,
                Some(crate::icons::Icon::Signal),
                label,
                Tone::Quiet,
                ui.available_width().min(FORM_CONTROL_WIDTH),
            )
        })
        .inner;
    if button.clicked() {
        if running {
            form.check.clear();
        } else if let Err(problems) = form.check.start(&form.draft, saved_local, ui.ctx().clone()) {
            form.problems = problems;
        }
    }
    if form.check.running() {
        ui.label(RichText::new("Checking…").small().color(MUTED));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    if let Some(rows) = &form.check.result {
        let failed = rows
            .iter()
            .filter(|row| row.outcome == check::Outcome::Failed)
            .count();
        let (summary, colour) = if failed > 0 {
            (format!("{failed} issue(s) found"), theme::DANGER_HOVER)
        } else {
            (String::from("Checks complete"), theme::SPEAKING)
        };
        ui.label(RichText::new(summary).small().color(colour));
        if ui
            .add(
                egui::Button::new(RichText::new("View results").color(theme::ACCENT_HOVER))
                    .frame(false),
            )
            .clicked()
        {
            form.results_open = true;
        }
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("Incoming calls still need a second-device test.")
            .small()
            .color(MUTED),
    );
}

fn settings(
    ui: &mut egui::Ui,
    form: &mut Form,
    width: f32,
    help: &mut HelpState<Help>,
    show_tooltips: bool,
    store: &mut Store,
) -> egui::Response {
    let before = form.draft.clone();
    let overridden = Settings::overridden(|name| std::env::var(name).ok());
    let response = card()
        .show(ui, |ui| {
            ui.set_width((width - 42.0).max(0.0));
            if !show_tooltips {
                ui.set_min_height(378.0);
            }
            setups::controls(ui, form, store, help, show_tooltips);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                ui.columns(2, |columns| {
                    let local = toggle_row(
                        &mut columns[0],
                        &mut form.draft.local_only,
                        "Local only",
                        "",
                    );
                    help.track(&local, Help::LocalOnly);
                    let nearby = columns[1]
                        .add_enabled_ui(form.draft.local_only, |ui| {
                            toggle_row(
                                ui,
                                &mut form.draft.nearby_discovery,
                                "Discoverable nearby",
                                "",
                            )
                        })
                        .inner;
                    help.track(&nearby, Help::Nearby);
                    if show_tooltips {
                        local.on_hover_text(LOCAL_HELP);
                        nearby
                            .on_hover_text(NEARBY_HELP)
                            .on_disabled_hover_text("Turn on Local only first.");
                    }
                });
            });
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                ui.columns(2, |columns| {
                    let router = columns[0]
                        .with_layout(Layout::top_down(Align::Center), |ui| {
                            ui.label(RichText::new("When hosting").small().color(MUTED));
                            ui.add_enabled_ui(!form.draft.local_only, |ui| {
                                toggle_row(
                                    ui,
                                    &mut form.draft.open_port,
                                    "Open a port on my router",
                                    "",
                                )
                            })
                            .inner
                        })
                        .inner;
                    help.track(&router, Help::Router);
                    if router.changed() {
                        form.draft.asked_open_port = true;
                    }
                    let hide = columns[1]
                        .with_layout(Layout::top_down(Align::Center), |ui| {
                            ui.label(RichText::new("When joining").small().color(MUTED));
                            toggle_row(ui, &mut form.draft.hide_address, "Hide my address", "")
                        })
                        .inner;
                    help.track(&hide, Help::Hide);
                    if show_tooltips {
                        router
                            .on_hover_text(ROUTER_HELP)
                            .on_disabled_hover_text("Router opening is ignored in Local only.");
                        hide.on_hover_text(HIDE_HELP);
                    }
                });
            });
            if form.draft.local_only && form.draft.hide_address {
                ui.label(
                    RichText::new(
                        "Choose either Local only or Hide my address. Local only has no relay.",
                    )
                    .small()
                    .color(theme::DANGER_HOVER),
                );
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            if form.draft.local_only && !form.draft.hide_address {
                ui.label(
                    RichText::new(
                        "Internet fields are inactive in Local only. Your values are kept.",
                    )
                    .small()
                    .color(MUTED),
                );
            }
            ui.add_space(8.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                for pair in Field::ALL.chunks(2) {
                    ui.columns(2, |columns| {
                        for (column, field) in columns.iter_mut().zip(pair) {
                            column.with_layout(Layout::top_down(Align::Center), |ui| {
                                ui.add_enabled_ui(
                                    !form.draft.local_only || *field == Field::Port,
                                    |ui| {
                                        field_input(
                                            ui,
                                            form,
                                            *field,
                                            &overridden,
                                            help,
                                            show_tooltips,
                                        );
                                    },
                                );
                            });
                        }
                    });
                    ui.add_space(12.0);
                }
            });
        })
        .response;
    if form.draft != before {
        form.saved = false;
        form.save_error = None;
    }
    response
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

const LOCAL_HELP: &str = "Call only devices on connected local networks. Ignores internet servers and router port opening, and disables update checks. Use a full invitation. A request already in flight may finish; restart Veil for a clean local-only session.";
const NEARBY_HELP: &str = "Needs Local only. Devices on this network can find your call and ask for its invitation using the six-digit code on the host's screen. The host still decides who may join.";

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

#[cfg(test)]
mod flow_tests {
    use super::*;

    #[test]
    fn activating_a_loaded_setup_clears_its_pending_removal() {
        let mut store = Store::open(None);
        let original = store.active_id();
        assert!(
            store
                .save_setup(None, "Work VPS", Settings::default())
                .is_ok()
        );
        let selected = store.active_id();
        assert!(store.activate(original).is_ok());
        let mut form = Form::new(&store);
        form.load_setup(&store, selected);
        assert!(form.dirty() && !form.edited());
        form.remove_confirm = true;

        assert!(form.save(&mut store));
        assert_eq!(store.active_id(), selected);
        assert!(!form.remove_confirm);
        assert!(!form.dirty());
        assert_eq!(store.setups().len(), 2);
    }

    #[test]
    fn loading_a_setup_is_only_a_draft_until_saved() {
        let mut store = Store::open(None);
        let default_id = store.active_id();
        let work = Settings {
            local_only: true,
            ..Settings::default()
        };
        assert!(store.save_setup(None, "Work VPS", work.clone()).is_ok());
        let work_id = store.active_id();
        assert!(store.activate(default_id).is_ok());
        let mut form = Form::new(&store);
        form.load_setup(&store, work_id);
        assert!(form.dirty() && !form.edited());
        assert_eq!(store.active_id(), default_id);
        assert!(!store.saved.local_only);
        assert_eq!(form.draft, work);
        form.setup_name = "Work network".into();
        assert!(form.save(&mut store));
        assert_eq!(store.active_id(), work_id);
        assert_eq!(store.active_name(), "Work network");
        assert!(!form.dirty());
    }

    #[test]
    fn failed_validation_preserves_the_unsaved_draft() {
        let mut store = Store::open(None);
        let mut form = Form::new(&store);
        form.draft.port = "70000".into();
        assert!(!form.save(&mut store));
        assert!(form.dirty());
        assert!(form.problems.iter().any(|(field, _)| *field == Field::Port));
        assert_eq!(store.saved, Settings::default());

        form.draft.port = "7710".into();
        assert!(form.save(&mut store));
        assert!(!form.dirty());
        assert!(form.problems.is_empty() && form.save_error.is_none());
    }

    #[test]
    fn privacy_conflict_keeps_both_choices_until_the_user_resolves_it() {
        let mut store = Store::open(None);
        let mut form = Form::new(&store);
        form.draft.local_only = true;
        form.draft.hide_address = true;
        assert!(!form.save(&mut store));
        assert!(form.dirty() && form.draft.local_only && form.draft.hide_address);
        assert_eq!(store.saved, Settings::default());
        form.draft.hide_address = false;
        assert!(form.save(&mut store));
        assert!(!form.dirty());
        assert!(store.saved.local_only && !store.saved.hide_address);
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::expect_used)]
mod layout_tests {
    use super::*;

    #[test]
    fn cards_fit_default_window_for_every_help_topic_and_results_state() {
        let ctx = egui::Context::default();
        theme::load_fonts(&ctx).expect("Windows UI fonts");
        theme::apply(&ctx);
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let topics = [
            Help::Overview,
            Help::LocalOnly,
            Help::Nearby,
            Help::Router,
            Help::Hide,
            Help::Field(Field::Rendezvous),
            Help::Field(Field::Forwarder),
            Help::Field(Field::Advertise),
            Help::Field(Field::Port),
            Help::Save,
            Help::Back,
            Help::Cancel,
        ];
        for (saved_local, conflict) in [(false, false), (true, false), (false, true)] {
            for has_results in [false, true] {
                for topic in topics {
                    let mut form = Form::new(&Store::open(None));
                    form.draft.local_only = conflict;
                    form.draft.hide_address = conflict;
                    if has_results {
                        form.check.result = Some(Vec::new());
                    }
                    let mut measured = None;
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(viewport),
                            ..Default::default()
                        },
                        |ui| {
                            let mut left = ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt("measure-form")
                                    .max_rect(egui::Rect::from_min_size(
                                        egui::pos2(24.0, 24.0),
                                        egui::vec2(CONNECTION_FORM_WIDTH, 700.0),
                                    ))
                                    .layout(Layout::top_down(Align::Center)),
                            );
                            let settings = settings(
                                &mut left,
                                &mut form,
                                CONNECTION_FORM_WIDTH,
                                &mut HelpState::default(),
                                false,
                                &mut Store::open(None),
                            );
                            let right_rect = egui::Rect::from_min_size(
                                settings.rect.right_top() + egui::vec2(PAGE_COLUMN_GAP, 0.0),
                                egui::vec2(HELP_WIDTH, settings.rect.height()),
                            );
                            let mut right = ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt("measure-helper")
                                    .max_rect(right_rect)
                                    .layout(Layout::top_down(Align::Center)),
                            );
                            let helper = connection_helper(
                                &mut right,
                                &mut form,
                                topic,
                                right_rect.height(),
                                saved_local,
                            );
                            measured = Some((settings.rect.height(), helper.rect.height()));
                        },
                    );
                    output.textures_delta.clear();
                    let (left, right) = measured.expect("measured cards");
                    assert!(
                        (left - right).abs() < 1.0,
                        "card heights differ: {left} / {right}; results={has_results}, saved_local={saved_local}, conflict={conflict}, topic={topic:?}"
                    );
                    // Allow 130 points for the heading, gaps, actions and status line.
                    assert!(
                        left.max(right) + 130.0 <= 752.0,
                        "default window would overflow: cards={left}/{right}, topic={topic:?}, local={saved_local}, conflict={conflict}"
                    );
                }
            }
        }
    }
}
