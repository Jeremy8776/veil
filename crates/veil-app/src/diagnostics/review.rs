//! An explicit, frozen preview before a report goes to the clipboard. Opening or refreshing
//! reads local evidence only; it does not test connectivity, read logs or upload anything.

use super::{Capture, Include, Snapshot, State, Step};
use crate::theme;
use eframe::egui::{self, RichText};

#[derive(Default)]
pub struct Review {
    #[cfg(feature = "dev-ui")]
    pub previewing: bool,
    pub call: Option<Snapshot>,
    pub check: Option<Snapshot>,
    pub open: bool,
    selected_check: bool,
    frozen: Option<Snapshot>,
    include: Include,
    preview: String,
    copied: bool,
}

impl Review {
    pub fn open(&mut self) {
        self.open = true;
        self.selected_check = self.call.is_none();
        self.include = Include::default();
        self.capture();
    }
    pub fn open_check(&mut self) {
        self.open();
        self.selected_check = true;
        self.capture();
    }
    fn capture(&mut self) {
        self.frozen = if self.selected_check {
            self.check.clone()
        } else {
            self.call.clone()
        };
        self.rebuild();
    }
    fn rebuild(&mut self) {
        self.preview = self
            .frozen
            .as_ref()
            .map(|s| s.export(self.include))
            .unwrap_or_default();
        self.copied = false;
    }
    fn copy_preview(&mut self, ctx: &egui::Context) {
        crate::mobile::copy(ctx, &self.preview);
        self.copied = true;
    }
    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let available = ctx.content_rect().size();
        if cfg!(target_os = "android") {
            // The phone's bottom sheet, tall and scrolling, instead of a desktop window.
            self.open = crate::mobile::sheet::show(ctx, "diagnostic-review", "Connection report", |ui| {
                egui::ScrollArea::vertical()
                    .max_height(available.y * 0.75)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| self.content(ui));
                    });
            });
            return;
        }
        egui::Window::new("Connection report")
            .id(egui::Id::unique("diagnostic-review"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width((available.x - 36.0).clamp(200.0, 540.0))
            .default_height((available.y - 110.0).max(200.0))
            .max_height((available.y - 70.0).max(180.0))
            .vscroll(true)
            .show(ctx, |ui| self.content(ui));
        self.open = open;
    }
    pub(super) fn content(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        #[cfg(feature = "dev-ui")]
        if self.previewing { ui.label(RichText::new("UI preview · synthetic evidence").color(theme::WARN)); }
        ui.label("Recorded on this device. Opening this report sends nothing.");
        if self.call.is_some() && self.check.is_some() {
            let before = self.selected_check;
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.selected_check, false, "Last / current call");
                ui.selectable_value(&mut self.selected_check, true, "Connection check");
            });
            if before != self.selected_check {
                self.include = Include::default();
                self.capture();
            }
        }
        let Some(snapshot) = self.frozen.as_ref() else {
            ui.label(RichText::new("No recorded evidence yet").font(theme::bold(17.0)));
            ui.label("Start a call or run Test these settings in Connection. This report will then be available here, including after the call ends.");
            return;
        };
        ui.label(
            RichText::new(if snapshot.report.ended {
                "Call ended"
            } else if snapshot.report.reconnecting {
                "Restoring the host connection"
            } else if snapshot.report.started {
                "Call evidence"
            } else {
                "Connection check evidence"
            })
            .font(theme::bold(20.0)),
        );
        let scope = snapshot.report.scope;
        ui.label(
            RichText::new(if scope.local_only {
                "Local only"
            } else if scope.relay_only {
                "Hide my address · relay only"
            } else {
                "Internet and local calls"
            })
            .color(theme::MUTED),
        );
        if let Some(problem) = snapshot.problem() {
            ui.label(RichText::new(problem.label()).color(theme::WARN));
            ui.label(problem.action());
        }
        ui.add_space(4.0);
        for step in Step::ALL {
            let state = snapshot.report.steps[step as usize];
            let colour = match state {
                State::Passed => theme::SPEAKING,
                State::Failed(_) => theme::WARN,
                _ => theme::MUTED,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(step.label()).strong());
                ui.label(RichText::new(state.label()).color(colour));
            });
        }
        ui.label(RichText::new("A helper answering does not prove hosting permission, admission or media delivery.").small().color(theme::MUTED));
        ui.add_space(4.0);
        ui.label(RichText::new(snapshot.media_label()).font(theme::bold(15.0)));
        for peer in snapshot.peers.iter().take(super::MAX_PEERS) {
            ui.label(format!(
                "Participant {} · {} · audio {} · video {}",
                peer.id,
                peer.route.label(),
                if peer.audio_recent {
                    "received"
                } else {
                    "not observed"
                },
                if peer.video_recent {
                    "received"
                } else {
                    "not observed"
                }
            ));
        }
        ui.label(
            RichText::new(snapshot.media_action())
                .small()
                .color(theme::MUTED),
        );
        ui.label(format!(
            "Camera: {} · Microphone: {}",
            capture_label(snapshot.camera),
            capture_label(snapshot.microphone)
        ));
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("Choose what to include").font(theme::bold(16.0)));
        ui.label("Addresses and device public keys start off. Raw logs, names, chat, invitations and private keys are never included.");
        let mut changed = ui
            .checkbox(
                &mut self.include.addresses,
                "Include candidate IP addresses and ports",
            )
            .changed();
        changed |= ui
            .checkbox(&mut self.include.device_keys, "Include device public keys")
            .changed();
        if changed {
            self.rebuild();
        }
        ui.label(RichText::new("The exact text copied is shown below. This is a snapshot; it does not retest the network.").small().color(theme::MUTED));
        egui::ScrollArea::vertical()
            .id_salt("report-preview")
            .max_height(180.0)
            .show(ui, |ui| {
                let mut shown = self.preview.as_str();
                let mut layouter = preview_layouter;
                ui.add(
                    egui::TextEdit::multiline(&mut shown)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width())
                        .desired_rows(8)
                        .layouter(&mut layouter),
                );
            });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Copy previewed report").clicked() {
                self.copy_preview(ui.ctx());
            }
            if ui.button("Capture latest snapshot").clicked() {
                self.capture();
            }
            if self.copied {
                ui.label(RichText::new("Report copied").color(theme::SPEAKING));
            }
        });
    }
}

/// Lays out the read-only report preview. epaint 0.37 can leave a row up to one glyph past the
/// wrap width when a word longer than the row (a device key) follows a space break, which
/// widened the window on phones depending on the key's characters. Reserving one monospace
/// glyph of wrap width keeps every row inside the box.
fn preview_layouter(
    ui: &egui::Ui,
    text: &dyn egui::TextBuffer,
    wrap_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let glyph = ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'W'));
    let job = egui::text::LayoutJob::simple(
        text.as_str().to_owned(),
        font,
        ui.visuals().text_color(),
        (wrap_width - glyph).max(glyph),
    );
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

fn capture_label(state: Capture) -> &'static str {
    match state {
        Capture::Off => "off",
        Capture::Starting => "waiting for frames",
        Capture::Active => "frames observed",
        Capture::PermissionDenied => "permission not granted",
        Capture::Unavailable => "unavailable",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn review_is_frozen_and_reopening_resets_sensitive_choices() {
        let mut review = Review {
            call: Some(super::super::tests::example()),
            ..Default::default()
        };
        review.open();
        let shown = review.preview.clone();
        review.call.as_mut().expect("call").report.reconnecting = true;
        assert_eq!(
            review.preview, shown,
            "engine updates cannot alter the reviewed copy"
        );
        review.capture();
        assert_ne!(review.preview, shown);
        review.include = Include {
            addresses: true,
            device_keys: true,
        };
        review.rebuild();
        assert!(review.preview.contains("192.0.2.7"));
        review.open();
        assert_eq!(review.include, Include::default());
        assert!(!review.preview.contains("192.0.2.7"));
    }

    #[test]
    fn copied_text_is_exactly_the_frozen_preview() {
        let mut review = Review {
            call: Some(super::super::tests::example()),
            ..Default::default()
        };
        review.open();
        let expected = review.preview.clone();
        review.call = None;
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            review.copy_preview(ui.ctx())
        });
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &expected)
        ));
        assert!(review.copied);
        output.textures_delta.clear();
    }

    #[test]
    fn report_content_fits_phone_and_desktop_widths() {
        let ctx = egui::Context::default();
        theme::load_fonts(&ctx).expect("system fonts");
        theme::apply(&ctx);
        for width in [264.0, 320.0, 540.0] {
            let mut review = Review {
                call: Some(super::super::tests::example()),
                ..Default::default()
            };
            review.open();
            review.include = Include {
                addresses: true,
                device_keys: true,
            };
            review.rebuild();
            for _ in 0..3 {
                let mut used = 0.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width + 32.0, 2400.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(
                            egui::Rect::from_min_size(
                                egui::pos2(8.0, 8.0),
                                egui::vec2(width, 2300.0),
                            ),
                        ));
                        review.content(&mut child);
                        used = child.min_rect().width();
                    },
                );
                output.textures_delta.clear();
                assert!(used <= width + 1.0, "report expanded {used} beyond {width}");
            }
        }
    }

    /// Device keys are random, and some keys used to wrap one glyph past the phone width.
    #[test]
    fn report_fits_a_phone_for_any_device_key() {
        let ctx = egui::Context::default();
        theme::load_fonts(&ctx).expect("system fonts");
        theme::apply(&ctx);
        let width = 264.0;
        for _ in 0..100 {
            let snapshot = super::super::tests::example();
            let key = veil_net::node_addr::key_text(&snapshot.own_device.expect("device"));
            let mut review = Review {
                call: Some(snapshot),
                ..Default::default()
            };
            review.open();
            review.include = Include {
                addresses: true,
                device_keys: true,
            };
            review.rebuild();
            let mut used = 0.0;
            for _ in 0..3 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width + 32.0, 2400.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(
                            egui::Rect::from_min_size(
                                egui::pos2(8.0, 8.0),
                                egui::vec2(width, 2300.0),
                            ),
                        ));
                        review.content(&mut child);
                        used = child.min_rect().width();
                    },
                );
                output.textures_delta.clear();
            }
            assert!(
                used <= width + 1.0,
                "key {key} expanded the report to {used}"
            );
        }
    }
}
