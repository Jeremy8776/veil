//! The explicit home-screen nearby scan. Nothing runs until the person starts a search;
//! listening stops on leaving Home or after 20 seconds. Fetches use throwaway device keys
//! and need the code shown on the host's screen.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use veil_net::nearby::{self, Found};
use veil_security::short_invite::InviteLink;

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
        } else {
            let find = ui
                .add_enabled_ui(!self.fetching, |ui| {
                    pill(
                        ui,
                        Some(crate::icons::Icon::Signal),
                        "Find nearby calls",
                        Tone::Quiet,
                        ui.available_width().min(theme::FORM_CONTROL_WIDTH),
                    )
                })
                .inner;
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
            ui.label(
                RichText::new("Nearby calls are unverified. The host still admits you.")
                    .small()
                    .color(MUTED),
            );
            for found in rows {
                if ui
                    .add_enabled(
                        !self.fetching,
                        egui::Button::new(format!("Request invite · {}", found.label())),
                    )
                    .clicked()
                {
                    self.chosen = Some(found);
                    self.code.clear();
                }
            }
        } else if searching {
            ui.label(
                RichText::new("Hosts must enable nearby discovery in Connection.")
                    .small()
                    .color(MUTED),
            );
        }
        if self.fetching {
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
    let width = ui.available_width().min(theme::FORM_CONTROL_WIDTH);
    let stop_width = 88.0_f32.min(width * 0.4);
    ui.allocate_ui_with_layout(
        egui::vec2(width, 40.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = theme::GAP;
            ui.allocate_ui_with_layout(
                egui::vec2(width - stop_width - theme::GAP, 40.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(egui::Spinner::new().size(16.0).color(MUTED));
                    ui.label(RichText::new("Searching nearby").small().color(MUTED));
                },
            );
            pill(ui, None, "Stop", Tone::Quiet, stop_width)
                .on_hover_text("Stop looking for calls on this local network.")
        },
    )
    .inner
}

impl Nearby {
    /// Asks for the code on the host's screen. Without it the host's service sends nothing.
    fn code_ui(&mut self, ui: &mut egui::Ui, found: Found) {
        ui.label(
            RichText::new(format!(
                "Type the nearby code shown on the host's screen for call {}.",
                found.label()
            ))
            .small()
            .color(MUTED),
        );
        let code = nearby::Code::parse(&self.code);
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.code)
                    .hint_text("123 456")
                    .desired_width(90.0)
                    .char_limit(7),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let get = ui
                .add_enabled(
                    code.is_some() && !self.fetching,
                    egui::Button::new("Get invite"),
                )
                .clicked();
            if (get || enter)
                && !self.fetching
                && let Some(code) = code
            {
                self.chosen = None;
                self.code.clear();
                self.fetch(found, code, ui.ctx().clone());
            }
            if ui.button("Cancel").clicked() {
                self.chosen = None;
                self.code.clear();
            }
        });
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
        let (text, colour) = if verified {
            ("This host is a device you verified.", MUTED)
        } else {
            (
                "You have not verified this host. Anyone on this network can announce a call.",
                theme::DANGER_HOVER,
            )
        };
        ui.label(RichText::new(text).small().color(colour));
        let mut chosen = None;
        ui.horizontal(|ui| {
            if ui
                .button("Continue")
                .on_hover_text(
                    "Go to the pre-join screen for this call. The host still admits you.",
                )
                .clicked()
            {
                chosen = Some(InviteLink::Full(invite.clone()));
            }
            if ui.button("Cancel").clicked() {
                self.confirm = None;
            }
        });
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
