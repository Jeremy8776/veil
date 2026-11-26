//! The explicit home-screen nearby scan. Nothing runs until the person starts a search;
//! listening stops on leaving Home or after 20 seconds. Fetches use throwaway device keys
//! and need the code shown on the host's screen.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use veil_net::nearby::{self, Found};
use veil_security::short_invite::InviteLink;

use crate::screens::dialog::{Choice, pair};
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, pill};

type Rows = Arc<Mutex<Vec<(Found, Instant)>>>;
type Answer = Arc<Mutex<Option<Result<veil_security::invite::Invite, String>>>>;

#[derive(Default)]
pub struct Nearby {
    stop: Option<Arc<AtomicBool>>,
    rows: Rows,
    answer: Answer,
    error: Arc<Mutex<Option<String>>>,
    fetching: bool,
    generation: Arc<AtomicU64>,
    /// A fetched invitation waiting for the person to confirm its host.
    confirm: Option<veil_security::invite::Invite>,
    /// The call picked from the list, waiting for the host's code.
    chosen: Option<Found>,
    code: String,
}

impl Drop for Nearby {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Nearby {
    /// Cancel listening and discard stale discovery state when leaving Home.
    pub fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.fetching = false;
        self.confirm = None;
        self.chosen = None;
        self.code.clear();
        if let Ok(mut answer) = self.answer.lock() {
            *answer = None;
        }
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Ok(mut rows) = self.rows.lock() {
            rows.clear();
        }
    }

    fn search(&mut self, ctx: egui::Context) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        let generation = self.generation.clone();
        let this_search = generation.load(Ordering::Relaxed);
        let (rows, error) = (self.rows.clone(), self.error.clone());
        if let Ok(mut error) = error.lock() {
            *error = None;
        }
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(runtime) => runtime.block_on(async {
                    let mut scanner = nearby::Scanner::start().map_err(|e| e.to_string())?;
                    let until = tokio::time::sleep(Duration::from_secs(20));
                    tokio::pin!(until);
                    let mut tick = tokio::time::interval(Duration::from_millis(200));
                    loop {
                        tokio::select! {
                            found = scanner.next() => {
                                let Some(found) = found else { break };
                                if let Ok(mut rows) = rows.lock() {
                                    if stop.load(Ordering::Relaxed) { break; }
                                    let now = Instant::now();
                                    rows.retain(|(_, at)| now.duration_since(*at) < nearby::EXPIRY);
                                    if let Some((_, at)) = rows.iter_mut().find(|(f, _)| *f == found) {
                                        *at = now;
                                    } else if rows.len() < 32 { rows.push((found, now)); }
                                }
                                ctx.request_repaint();
                            }
                            _ = tick.tick() => if stop.load(Ordering::Relaxed) { break },
                            _ = &mut until => break,
                        }
                    }
                    Ok::<_, String>(())
                }),
                Err(e) => Err(format!("Cannot start nearby search: {e}")),
            };
            if let Err(problem) = result
                && let Ok(mut error) = error.lock()
                && generation.load(Ordering::Relaxed) == this_search
            {
                *error = Some(problem);
            }
            stop.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn fetch(&mut self, found: Found, code: nearby::Code, ctx: egui::Context) {
        self.fetching = true;
        let answer = self.answer.clone();
        let generation = self.generation.clone();
        let this_fetch = generation.load(Ordering::Relaxed);
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|runtime| {
                    runtime
                        .block_on(nearby::fetch(
                            &found,
                            code,
                            crate::screens::invite::now_unix(),
                        ))
                        .map_err(|e| e.to_string())
                });
            if let Ok(mut answer) = answer.lock()
                && generation.load(Ordering::Relaxed) == this_fetch
            {
                *answer = Some(result);
            }
            ctx.request_repaint();
        });
    }

    /// `verified` says whether a host key is one the person verified before. A nearby result is
    /// never trusted by itself: anyone on the network can announce a call, so the person sees
    /// whether this host is someone they verified and chooses to go on.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        verified: impl Fn(&veil_security::PublicId) -> bool,
    ) -> Option<InviteLink> {
        if cfg!(target_os = "android") {
            return self.body(ui, verified);
        }
        // The desktop's controls share one centre line and one width.
        ui.vertical_centered(|ui| self.body(ui, verified)).inner
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        verified: impl Fn(&veil_security::PublicId) -> bool,
    ) -> Option<InviteLink> {
        if let Some(invite) = self.confirm.clone() {
            let known = verified(&invite_host(&invite));
            return self.confirm_ui(ui, invite, known);
        }
        let answer = self.answer.lock().ok().and_then(|mut a| a.take());
        if let Some(answer) = answer {
            self.fetching = false;
            match answer {
                Ok(invite) => {
                    // Stop listening, then ask before anything is sent to this host.
                    self.stop();
                    self.confirm = Some(invite);
                    ui.ctx().request_repaint();
                    return None;
                }
                Err(problem) => {
                    if let Ok(mut error) = self.error.lock() {
                        *error = Some(problem);
                    }
                }
            }
        }
        let searching = self
            .stop
            .as_ref()
            .is_some_and(|s| !s.load(Ordering::Relaxed));
        ui.add_space(8.0);
        if searching {
            if search_control(ui).clicked() {
                self.stop();
            }
        } else if cfg!(target_os = "android") {
            // On the phone this is a secondary choice, so it is a quiet link, not a button.
            let find = ui
                .vertical_centered(|ui| {
                    ui.add_enabled_ui(!self.fetching, |ui| {
                        crate::mobile::widgets::link(
                            ui,
                            // Not Signal: that is the connection icon in the top-right corner.
                            Some(crate::icons::Icon::People),
                            "Find nearby calls",
                        )
                        .on_hover_text("Calls on the same Wi-Fi or hotspot")
                    })
                    .inner
                })
                .inner;
            if find.clicked() {
                self.search(ui.ctx().clone());
            }
        } else if self.fetching {
            busy(ui, "Getting invite…", false)
                .on_disabled_hover_text("Asking the host for the invitation.");
        } else {
            let find = pill(
                ui,
                Some(crate::icons::Icon::Signal),
                "Find nearby calls",
                Tone::Quiet,
                width(ui),
            )
            .on_hover_text(SEARCH_HINT);
            if find.clicked() {
                self.search(ui.ctx().clone());
            }
        }
        let rows = self
            .rows
            .lock()
            .map(|mut rows| {
                rows.retain(|(_, seen)| seen.elapsed() < nearby::EXPIRY);
                rows.iter()
                    .map(|(found, _)| found.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(found) = self.chosen.clone() {
            self.code_ui(ui, found);
        } else if !rows.is_empty() {
            if cfg!(target_os = "android") {
                ui.label(
                    RichText::new("Nearby calls are unverified. The host still admits you.")
                        .small()
                        .color(MUTED),
                );
            }
            for found in rows {
                if cfg!(target_os = "android") {
                    if ui
                        .add_enabled_ui(!self.fetching, |ui| {
                            crate::mobile::action(
                                ui,
                                None,
                                &format!("Request invite · {}", found.label()),
                                Tone::Quiet,
                            )
                        })
                        .inner
                    {
                        self.chosen = Some(found);
                        self.code.clear();
                    }
                    continue;
                }
                let pick = ui
                    .add_enabled_ui(!self.fetching, |ui| {
                        let icon = Some(crate::icons::Icon::People);
                        pill(ui, icon, &found.label(), Tone::Quiet, width(ui))
                    })
                    .inner
                    .on_hover_text(
                        "Ask this host for an invite. Nearby calls are unverified, and the \
                         host still admits you.",
                    );
                if pick.clicked() {
                    self.chosen = Some(found);
                    self.code.clear();
                }
            }
        }
        if self.fetching && cfg!(target_os = "android") {
            ui.label("Requesting the invitation…");
        }
        if let Some(error) = self.error.lock().ok().and_then(|error| error.clone()) {
            ui.label(RichText::new(error).small().color(theme::DANGER_HOVER));
        }
        if searching || self.fetching {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        None
    }
}

/// Keep the running search on the same centred, 40-point control row as its idle button.
fn search_control(ui: &mut egui::Ui) -> egui::Response {
    if cfg!(target_os = "android") {
        // Same centred spot as the idle "Find nearby calls" link: spinner, status, Cancel.
        let label = RichText::new("Searching…").size(16.0).color(MUTED);
        let measure = |ui: &egui::Ui, text: &str| {
            ui.painter()
                .layout_no_wrap(text.into(), egui::FontId::proportional(16.0), MUTED)
                .size()
                .x
        };
        let text_width = measure(ui, "Searching…");
        let cancel_width = measure(ui, "Cancel") + 24.0;
        let total = 16.0 + 8.0 + text_width + 8.0 + cancel_width;
        // A full-height row so the 48-point Cancel link and the label share one centre line.
        let row = egui::vec2(ui.available_width(), 48.0);
        return ui
            .allocate_ui_with_layout(
                row,
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_space(((ui.available_width() - total) * 0.5).max(0.0));
                    ui.add(egui::Spinner::new().size(16.0).color(MUTED));
                    ui.label(label)
                        .on_hover_text("Hosts must turn on nearby discovery in Connection.");
                    crate::mobile::widgets::link(ui, None, "Cancel")
                },
            )
            .inner;
    }
    busy(ui, "Stop searching", true).on_hover_text(SEARCH_HINT)
}

const SEARCH_HINT: &str = "Looks for calls on the same Wi-Fi or hotspot for 20 seconds. \
                           Hosts turn on nearby discovery in Connection.";

/// The width of every desktop nearby control: the home form's control width.
fn width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(theme::FORM_CONTROL_WIDTH)
}

/// A quiet pill with a spinner just before its centred label, for work in progress. Clickable
/// only when `enabled`.
fn busy(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let response = ui
        .add_enabled_ui(enabled, |ui| pill(ui, None, label, Tone::Quiet, width(ui)))
        .inner;
    let text = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::bold(14.0), theme::TEXT)
        .size()
        .x;
    egui::Spinner::new()
        .size(SPINNER)
        .color(MUTED)
        .paint_at(ui, spinner_rect(response.rect, text));
    response
}

const SPINNER: f32 = 14.0;

/// Where the spinner goes in a pill `rect` whose centred label is `text` wide: one gap to the
/// label's left, on the pill's centre line.
fn spinner_rect(rect: egui::Rect, text: f32) -> egui::Rect {
    let right = rect.center().x - text / 2.0 - theme::GAP;
    egui::Rect::from_center_size(
        egui::pos2(right - SPINNER / 2.0, rect.center().y),
        egui::vec2(SPINNER, SPINNER),
    )
}

impl Nearby {
    /// Asks for the code on the host's screen. Without it the host's service sends nothing.
    fn code_ui(&mut self, ui: &mut egui::Ui, found: Found) {
        if cfg!(target_os = "android") {
            ui.label(
                RichText::new(format!(
                    "Type the nearby code shown on the host's screen for call {}.",
                    found.label()
                ))
                .small()
                .color(MUTED),
            );
        } else {
            ui.label(RichText::new(format!("Code for {}", found.label())).color(MUTED));
        }
        let code = nearby::Code::parse(&self.code);
        if cfg!(target_os = "android") {
            let field = ui.add_sized(
                [ui.available_width(), 48.0],
                egui::TextEdit::singleline(&mut self.code)
                    .hint_text("123 456")
                    .char_limit(7),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut get = false;
            ui.columns(2, |columns| {
                get = columns[0]
                    .add_enabled_ui(code.is_some() && !self.fetching, |ui| {
                        crate::mobile::action(ui, None, "Get invite", Tone::Primary)
                    })
                    .inner;
                if crate::mobile::action(&mut columns[1], None, "Cancel", Tone::Quiet) {
                    self.chosen = None;
                    self.code.clear();
                }
            });
            if (get || enter)
                && !self.fetching
                && let Some(code) = code
            {
                self.chosen = None;
                self.code.clear();
                self.fetch(found, code, ui.ctx().clone());
            }
            return;
        }
        let width = width(ui);
        let field = ui
            .add_sized(
                [width, 40.0],
                egui::TextEdit::singleline(&mut self.code)
                    .align(egui::Align2::CENTER_CENTER)
                    .font(theme::bold(16.0))
                    .hint_text("123 456")
                    .char_limit(7),
            )
            .on_hover_text(
                "The nearby code on the host's screen. Without it the host sends nothing.",
            );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ready = code.is_some() && !self.fetching;
        let (cancel, get) = pair(
            ui,
            width,
            Choice::new(None, "Cancel", Tone::Quiet),
            Choice::new(None, "Get invite", Tone::Primary).enabled(ready),
        );
        if (get.clicked() || enter)
            && !self.fetching
            && let Some(code) = code
        {
            self.chosen = None;
            self.code.clear();
            self.fetch(found, code, ui.ctx().clone());
        }
        if cancel.clicked() {
            self.chosen = None;
            self.code.clear();
        }
    }
}

fn invite_host(invite: &veil_security::invite::Invite) -> veil_security::PublicId {
    invite.host
}

impl Nearby {
    fn confirm_ui(
        &mut self,
        ui: &mut egui::Ui,
        invite: veil_security::invite::Invite,
        verified: bool,
    ) -> Option<InviteLink> {
        ui.add_space(8.0);
        let mut chosen = None;
        if cfg!(target_os = "android") {
            let (text, colour) = if verified {
                ("This host is a device you verified.", MUTED)
            } else {
                (
                    "You have not verified this host. Anyone on this network can announce a call.",
                    theme::DANGER_HOVER,
                )
            };
            ui.label(RichText::new(text).small().color(colour));
            ui.columns(2, |columns| {
                if crate::mobile::action(&mut columns[0], None, "Continue", Tone::Primary) {
                    chosen = Some(InviteLink::Full(invite.clone()));
                }
                if crate::mobile::action(&mut columns[1], None, "Cancel", Tone::Quiet) {
                    self.confirm = None;
                }
            });
            if chosen.is_some() {
                self.confirm = None;
            }
            return chosen;
        }
        let (label, colour, hint) = if verified {
            (
                "Verified host",
                theme::SPEAKING,
                "This host is a device you verified.",
            )
        } else {
            (
                "Unverified host",
                theme::WARN,
                "You have not verified this host. Anyone on this network can announce a call, \
                 so continue only if you expect it.",
            )
        };
        ui.label(RichText::new(label).font(theme::bold(14.0)).color(colour))
            .on_hover_text(hint);
        let (cancel, go) = pair(
            ui,
            width(ui),
            Choice::new(None, "Cancel", Tone::Quiet),
            Choice::new(None, "Continue", Tone::Primary)
                .hint("Go to the pre-join screen for this call. The host still admits you."),
        );
        if go.clicked() {
            chosen = Some(InviteLink::Full(invite.clone()));
        }
        if cancel.clicked() {
            self.confirm = None;
        }
        if chosen.is_some() {
            self.confirm = None;
        }
        chosen
    }
}

/// Starts the host's explicitly enabled nearby service. The callback always reads the
/// newest full link, so an already consumed single-use invitation is not kept in the service.
pub fn advertise(shared: Arc<crate::engine::Shared>) -> Result<nearby::Advertiser, String> {
    nearby::Advertiser::start(Arc::new(move || {
        let view = shared.view.lock().ok()?;
        (!view.invite_full.is_empty()).then(|| view.invite_full.clone())
    }))
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spinner_sits_one_gap_left_of_the_label_on_the_centre_line() {
        let pill = egui::Rect::from_min_size(egui::pos2(100.0, 20.0), egui::vec2(240.0, 40.0));
        let spinner = spinner_rect(pill, 100.0);
        assert_eq!(spinner.center().y, pill.center().y);
        assert_eq!(spinner.right(), pill.center().x - 50.0 - theme::GAP);
        assert_eq!(spinner.width(), SPINNER);
        assert!(spinner.left() > pill.left());
    }
}
