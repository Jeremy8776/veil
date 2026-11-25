//! The phone call screen's control bar, with screen sharing (SHARE-02).
//!
//! Purpose: the bottom bar of the phone call: mute, video, share, flip and leave. Share opens a
//! short sheet with the phone sound choice and what Android will not capture, then Android's
//! own consent prompt, which picks the whole screen or one app where the phone supports that.
//! While the share includes sound, a strip above the buttons gives its level and mute. The
//! share itself runs on the desktop share path (`share.rs`), so encoding, sealing and the wire
//! format are the same as desktop.
//!
//! Invariants:
//! - Nothing is shared until Android's consent prompt says yes. Saying no changes nothing.
//! - "Include phone sound" starts off for every share and is greyed out, with the reason,
//!   where it cannot work.
//! - Stop share here, Stop sharing in the notification and Android's own indicator all end
//!   the share. Leaving the call ends it too (`app.rs`).

use eframe::egui::{self, Margin};

use super::widgets as w;
use super::{action, setting};
use crate::app::VeilApp;
use crate::icons::Icon;
use crate::screens::{call::CallView, call_menus, controls};
use crate::share::{SoundState, SoundSupport};
use crate::theme;
use crate::volume::SPEAKER_MAX;
use crate::widgets::Tone;

const NOT_CAPTURED: &str = "Apps that block capture stay silent, and protected screens show as \
                            black. Locking your phone stops sharing.";

/// Where starting a share stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sheet {
    /// The sheet is open, before Android's prompt.
    Asking { with_sound: bool },
    /// Android's consent prompt is open.
    Waiting { with_sound: bool },
}

fn sheet_id() -> egui::Id {
    egui::Id::unique("mobile-share-sheet")
}

/// Draws the control bar and the share sheet. False once Leave is pressed.
pub fn show(view: &mut CallView, app: &mut VeilApp, ui: &mut egui::Ui) -> bool {
    let ctx = ui.ctx().clone();
    let mut sheet = ctx
        .data(|data| data.get_temp::<Sheet>(sheet_id()))
        .and_then(|sheet| step(sheet, view, app, &ctx));
    let mut stay = true;
    egui::Panel::bottom("mobile-call-controls")
        .frame(
            egui::Frame::new()
                .fill(theme::SURFACE)
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            let sound = app.share.sound.state();
            if app.share.is_sharing() && sound.is_on() {
                sound_strip(ui, &sound);
                ui.add_space(8.0);
            }
            ui.columns(5, |columns| {
                let mic = app.media.mic_on();
                let label = if mic { "Mute" } else { "Unmute" };
                if w::call_control(&mut columns[0], Icon::Mic, label, !mic, false).clicked() {
                    controls::toggle_mic(app);
                }
                let camera = app.media.camera_on();
                let label = if camera { "Stop video" } else { "Start video" };
                if w::call_control(&mut columns[1], Icon::Video, label, !camera, false).clicked() {
                    controls::toggle_camera(app);
                }
                let sharing = app.share.is_sharing();
                let label = if sharing { "Stop share" } else { "Share" };
                if w::call_control(&mut columns[2], Icon::Share, label, false, sharing).clicked() {
                    if sharing {
                        app.share.stop();
                        view.notify("Screen sharing stopped.");
                    } else if sheet.is_none() {
                        sheet = Some(Sheet::Asking { with_sound: false });
                    }
                }
                if w::call_control(&mut columns[3], Icon::Refresh, "Flip", false, false).clicked() {
                    super::flip(app);
                }
                if w::call_control(&mut columns[4], Icon::Leave, "Leave", false, true).clicked() {
                    stay = false;
                }
            });
        });
    if !stay {
        sheet = None;
    }
    ctx.data_mut(|data| match sheet {
        Some(sheet) => {
            data.insert_temp(sheet_id(), sheet);
        }
        None => data.remove::<Sheet>(sheet_id()),
    });
    stay
}

/// Moves the share sheet on by one frame. `None` once it is closed.
fn step(sheet: Sheet, view: &mut CallView, app: &mut VeilApp, ctx: &egui::Context) -> Option<Sheet> {
    match sheet {
        Sheet::Waiting { with_sound } => match app.share.take_chosen() {
            Some(Ok(Some(source))) => {
                app.share.start(source, None, with_sound);
                None
            }
            Some(Ok(None)) => {
                app.share.close_picker();
                view.notify("Screen sharing was not started.");
                None
            }
            Some(Err(())) => {
                app.share.close_picker();
                view.notify("Android could not start screen sharing.");
                None
            }
            None => Some(sheet),
        },
        Sheet::Asking { mut with_sound } => {
            let support = app.share.sound.support();
            let why = sound_unavailable(support.as_ref());
            if why.is_some() {
                with_sound = false;
            }
            let mut next = Some(Sheet::Asking { with_sound });
            let open = super::sheet::show(ctx, "share-sheet", "Share screen", |ui| {
                // Android's own prompt asks what to share; its limits stay in the tooltip.
                ui.add_enabled_ui(why.is_none(), |ui| {
                    let detail = why.unwrap_or("Send what other apps play. Call sound is left out.");
                    setting(ui, &mut with_sound, "Include phone sound", detail);
                })
                .response
                .on_hover_text(NOT_CAPTURED);
                ui.add_space(16.0);
                next = Some(Sheet::Asking { with_sound });
                if action(ui, Some(Icon::Share), "Continue", Tone::Primary) {
                    app.share.choose_on_desktop();
                    next = Some(Sheet::Waiting { with_sound });
                }
            });
            if open { next } else { None }
        }
    }
}

/// Why phone sound cannot be included, if it cannot.
fn sound_unavailable(support: Option<&SoundSupport>) -> Option<&'static str> {
    match support {
        Some(SoundSupport::Ready) => None,
        Some(SoundSupport::Unavailable(why)) => Some(why),
        None => Some("Checking…"),
    }
}

/// The share's sound: says it is going out, with the level and mute everyone hears.
fn sound_strip(ui: &mut egui::Ui, sound: &SoundState) {
    let muted = sound.muted();
    let text = if muted { "Phone sound muted" } else { "Sending phone sound" };
    w::label(ui, text, 13.0, false, theme::MUTED);
    ui.horizontal(|ui| {
        let button = if muted { "Unmute" } else { "Mute" };
        if w::action(ui, Some(Icon::Volume), button, Tone::Quiet, 120.0).clicked() {
            sound.set_muted(!muted);
        }
        let mut level = sound.level();
        let hint = "How loud your phone's sound is for everyone";
        if call_menus::volume(ui, "Phone sound level", &mut level, SPEAKER_MAX, hint) {
            sound.set_level(level);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_sound_is_offered_only_when_it_can_work() {
        assert_eq!(sound_unavailable(Some(&SoundSupport::Ready)), None);
        let why = SoundSupport::Unavailable("Needs microphone permission");
        assert_eq!(sound_unavailable(Some(&why)), Some("Needs microphone permission"));
        assert_eq!(sound_unavailable(None), Some("Checking…"), "not offered until known");
    }
}
