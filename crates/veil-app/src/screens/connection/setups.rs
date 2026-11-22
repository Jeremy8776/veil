//! Editing a saved connection never activates it before Save changes.

use super::*;

pub(super) fn controls(
    ui: &mut egui::Ui,
    form: &mut Form,
    store: &mut Store,
    help: &mut HelpState<Help>,
    show_tooltips: bool,
) {
    let edited = form.edited();
    let mut selected = form.setup_id;
    ui.columns(2, |columns| {
        columns[0].with_layout(Layout::top_down(Align::Center), |ui| {
            ui.label(RichText::new("Saved setup").color(MUTED));
            let response = ui
                .add_enabled_ui(!edited, |ui| {
                    egui::ComboBox::from_id_salt("connection-setup")
                        .truncate()
                        .width(ui.available_width().min(FORM_CONTROL_WIDTH))
                        .selected_text(if form.setup_id.is_none() {
                            "New setup"
                        } else {
                            &form.baseline_name
                        })
                        .show_ui(ui, |ui| {
                            for setup in store.setups() {
                                let label = if setup.id == store.active_id() {
                                    format!("{} · Active", setup.name)
                                } else {
                                    setup.name.clone()
                                };
                                ui.selectable_value(&mut selected, Some(setup.id), label);
                            }
                        })
                        .response
                })
                .inner
                .on_disabled_hover_text("Save or discard edits before loading another setup.");
            track_setup_help(&response, help, show_tooltips);
        });
        columns[1].with_layout(Layout::top_down(Align::Center), |ui| {
            let label = ui.label(RichText::new("Setup name").color(MUTED));
            let response = ui
                .add(
                    TextEdit::singleline(&mut form.setup_name)
                        .align(egui::Align2::CENTER_CENTER)
                        .hint_text("Work VPS")
                        .char_limit(crate::connection::MAX_SETUP_NAME)
                        .desired_width(ui.available_width().min(FORM_CONTROL_WIDTH)),
                )
                .labelled_by(label.id);
            if response.changed() {
                form.saved = false;
                form.save_error = None;
            }
            track_setup_help(&response, help, show_tooltips);
        });
    });
    if selected != form.setup_id
        && let Some(id) = selected
    {
        form.load_setup(store, id);
    }
    ui.add_space(4.0);
    if form.edited() {
        form.remove_confirm = false;
    }
    if form.remove_confirm {
        ui.allocate_ui_with_layout(
            egui::vec2(352.0, 32.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(RichText::new("Remove this setup?").small());
                if ui
                    .add(
                        egui::Button::new(RichText::new("Remove").color(theme::DANGER_HOVER))
                            .min_size(egui::vec2(88.0, 32.0)),
                    )
                    .clicked()
                    && let Some(id) = form.setup_id
                {
                    match store.remove_setup(id) {
                        Ok(()) => form.load_setup(store, store.active_id()),
                        Err(problem) => form.save_error = Some(problem),
                    }
                }
                if ui
                    .add(egui::Button::new("Keep setup").min_size(egui::vec2(88.0, 32.0)))
                    .clicked()
                {
                    form.remove_confirm = false;
                }
            },
        );
        return;
    }
    ui.allocate_ui_with_layout(
        egui::vec2(352.0, 32.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            let add = ui.add_enabled(
                !edited && store.setups().len() < crate::connection::MAX_SETUPS,
                egui::Button::new("New setup").min_size(egui::vec2(112.0, 32.0)),
            );
            track_setup_help(&add, help, show_tooltips);
            if add.clicked() {
                form.setup_id = None;
                form.setup_name.clear();
                form.remove_confirm = false;
                form.saved = false;
                form.check.clear();
            }
            let removable = !edited && form.setup_id.is_some_and(|id| id != store.active_id());
            let remove = ui
                .add_enabled(
                    removable,
                    egui::Button::new("Remove setup").min_size(egui::vec2(112.0, 32.0)),
                )
                .on_disabled_hover_text(
                    "Load an inactive setup with no unsaved edits to remove it.",
                );
            track_setup_help(&remove, help, show_tooltips);
            if remove.clicked() {
                form.remove_confirm = !form.remove_confirm;
            }
            let discard = ui.add_enabled(
                edited,
                egui::Button::new("Discard edits").min_size(egui::vec2(112.0, 32.0)),
            );
            if discard.clicked() {
                form.load_setup(store, form.setup_id.unwrap_or(store.active_id()));
            }
            track_setup_help(&discard, help, show_tooltips);
        },
    );
}

fn track_setup_help(response: &egui::Response, help: &mut HelpState<Help>, show_tooltips: bool) {
    help.track(response, Help::Setup);
    if show_tooltips {
        response.clone().on_hover_text(Help::Setup.text().1);
    }
}
