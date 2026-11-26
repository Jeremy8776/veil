//! An explicit, frozen preview before a report goes to the clipboard. Opening or refreshing
//! reads local evidence only; it does not test connectivity, read logs or upload anything.

use super::{Capture, Include, Snapshot, State, Step};
use crate::icons::Icon;
use crate::screens::dialog::{Choice, pair};
use crate::theme;
use crate::widgets::{Tone, toggle_row};
use eframe::egui::{self, Align, Layout, RichText};

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
        let available = ctx.content_rect().size();
        if cfg!(target_os = "android") {
            // The phone's bottom sheet, tall and scrolling, instead of a desktop window.
            self.open =
                crate::mobile::sheet::show(ctx, "diagnostic-review", "Connection report", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(available.y * 0.75)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                self.content(ui)
                            });
                        });
                });
            return;
        }
        let width = crate::screens::dialog::width(available.x, WIDTH);
        self.open = crate::screens::dialog::show(
            ctx,
            "diagnostic-review",
            "Connection report",
            width,
            |ui| {
                self.content(ui);
            },
        );
    }
    /// One centred column: the source switch, the status, the steps, media, what to include,
    /// the exact text and the actions. Detail lives in tooltips.
    pub(super) fn content(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(Layout::top_down(Align::Center), |ui| self.column(ui));
    }
    fn column(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = theme::GAP;
        let width = ui.available_width();
        #[cfg(feature = "dev-ui")]
        if self.previewing {
            ui.label(RichText::new("UI preview · synthetic evidence").color(theme::WARN));
        }
        if self.call.is_some() && self.check.is_some() {
            self.source(ui, width);
        }
        let Some(snapshot) = self.frozen.as_ref() else {
            ui.add_space(12.0);
            ui.label(RichText::new("Nothing recorded yet").font(theme::bold(17.0)))
                .on_hover_text(SENDS_NOTHING);
            ui.label(RichText::new("Start a call or test your connection.").color(theme::MUTED));
            return;
        };
        ui.label(RichText::new(heading(snapshot)).font(theme::bold(20.0)))
            .on_hover_text(SENDS_NOTHING);
        ui.label(RichText::new(scope_label(snapshot)).color(theme::MUTED));
        if let Some(problem) = snapshot.problem() {
            ui.label(RichText::new(problem.label()).color(theme::WARN))
                .on_hover_text(problem.action());
        }
        group(ui, width, |ui| {
            for step in Step::ALL {
                let state = snapshot.report.steps[step as usize];
                ui.label(pair_text(step.label(), state.label(), state_colour(state)));
            }
        })
        .on_hover_text(
            "A helper answering does not prove hosting permission, admission or media delivery.",
        );
        group(ui, width, |ui| {
            ui.label(RichText::new(snapshot.media_label()).font(theme::bold(15.0)));
            for peer in snapshot.peers.iter().take(super::MAX_PEERS) {
                let who = format!("Participant {} · {}", peer.id, peer.route.label());
                let media = peer_media(peer.audio_recent, peer.video_recent);
                ui.label(pair_text(&who, media, theme::MUTED));
            }
            ui.label(
                RichText::new(format!(
                    "Camera {} · Microphone {}",
                    capture_label(snapshot.camera),
                    capture_label(snapshot.microphone)
                ))
                .color(theme::MUTED),
            );
        })
        .on_hover_text(snapshot.media_action());
        ui.add_space(4.0);
        ui.label(RichText::new("Include").font(theme::bold(14.0)))
            .on_hover_text("Both start off. Raw logs, names, chat, invitations and private keys are never included.");
        let mut changed = toggle_row(
            ui,
            &mut self.include.addresses,
            "IP addresses and ports",
            "",
        )
        .on_hover_text("The candidate addresses and ports this device tried.")
        .changed();
        changed |= toggle_row(ui, &mut self.include.device_keys, "Device public keys", "")
            .on_hover_text("Public keys identify devices. They cannot be used to sign in.")
            .changed();
        if changed {
            self.rebuild();
        }
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
                )
                .on_hover_text("Exactly what Copy puts on the clipboard. A snapshot: it does not retest the network.");
            });
        let copy_label = if self.copied { "Copied" } else { "Copy report" };
        let (refresh, copy) = pair(
            ui,
            width,
            Choice::new(Some(Icon::Refresh), "Refresh", Tone::Quiet)
                .hint("Take a new snapshot of what this device recorded."),
            Choice::new(Some(Icon::Copy), copy_label, Tone::Primary)
                .hint("Copies the text above, nothing more."),
        );
        if refresh.clicked() {
            self.capture();
        }
        if copy.clicked() {
            self.copy_preview(ui.ctx());
        }
    }
    /// Two equal buttons to choose between the call's report and the connection check's.
    fn source(&mut self, ui: &mut egui::Ui, width: f32) {
        let tone = |on: bool| if on { Tone::Primary } else { Tone::Quiet };
        let (call, check) = pair(
            ui,
            width,
            Choice::new(None, "Call", tone(!self.selected_check)).hint("The last or current call."),
            Choice::new(None, "Check", tone(self.selected_check))
                .hint("The last Test these settings in Connection."),
        );
        let wanted = if call.clicked() {
            Some(false)
        } else if check.clicked() {
            Some(true)
        } else {
            None
        };
        if let Some(wanted) = wanted
            && wanted != self.selected_check
        {
            self.selected_check = wanted;
            self.include = Include::default();
            self.capture();
        }
    }
}

const WIDTH: f32 = 460.0;
const SENDS_NOTHING: &str = "Recorded on this device. Opening this report sends nothing.";

fn heading(snapshot: &Snapshot) -> &'static str {
    let report = &snapshot.report;
    if report.ended {
        "Call ended"
    } else if report.reconnecting {
        "Reconnecting to the host"
    } else if report.started {
        "Call in progress"
    } else {
        "Connection check"
    }
}

fn scope_label(snapshot: &Snapshot) -> &'static str {
    let scope = snapshot.report.scope;
    if scope.local_only {
        "Local only"
    } else if scope.relay_only {
        "Hide my address · relay only"
    } else {
        "Internet and local calls"
    }
}

fn state_colour(state: State) -> egui::Color32 {
    match state {
        State::Passed => theme::SPEAKING,
        State::Failed(_) => theme::WARN,
        _ => theme::MUTED,
    }
}

/// What arrived recently from one participant, in three words or fewer.
fn peer_media(audio: bool, video: bool) -> &'static str {
    match (audio, video) {
        (true, true) => "audio and video",
        (true, false) => "audio only",
        (false, true) => "video only",
        (false, false) => "nothing yet",
    }
}

/// `name · value` on one centred line, the value in `colour`.
fn pair_text(name: &str, value: &str, colour: egui::Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::proportional(13.5);
    job.append(
        name,
        0.0,
        egui::TextFormat::simple(font.clone(), theme::TEXT),
    );
    job.append(
        " · ",
        0.0,
        egui::TextFormat::simple(font.clone(), theme::MUTED),
    );
    job.append(value, 0.0, egui::TextFormat::simple(font, colour));
    job.halign = Align::Center;
    job
}

/// A quiet rounded panel `width` wide holding centred lines. Its response carries the tooltip.
fn group(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    egui::Frame::new()
        .fill(theme::PAGE)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(width - 24.0);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                add(ui);
            });
        })
        .response
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
