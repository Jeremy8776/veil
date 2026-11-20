//! The microphone thread: reads 20 ms of sound at a time and hands it on.
//!
//! Purpose: each frame goes through noise suppression ([`Denoiser`], while
//! `Shared::suppress_noise` is on) and then the microphone volume, in that order, so the model
//! hears the microphone as it is. The result feeds the level meter and, during a call, the
//! engine's audio tap.
//!
//! Invariants:
//! - Turning suppression back on resets the model, so it starts from current sound.
//! - A device error is kept as text for the interface and the thread ends.
//! - The tap never blocks capture: a full engine queue drops the frame.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use veil_media::Denoiser;

use super::{Shared, lock, meter, which};

pub(super) fn mic_loop(shared: &Shared, ctx: &egui::Context, stop: &AtomicBool, id: Option<&str>) {
    use veil_media::AudioSettings;
    use veil_media::platform::Microphone;
    let mut mic = match Microphone::open(&AudioSettings::voice(), id) {
        Ok(mic) => {
            crate::note!("microphone: opened {}", which(id));
            mic
        }
        Err(e) => {
            crate::note!("microphone: cannot open {}: {e}", which(id));
            *lock(&shared.mic_error) = Some(e.to_string());
            ctx.request_repaint();
            return;
        }
    };
    let mut frames = 0u32;
    // The frame after noise suppression and the volume; reused so they cost no allocation.
    let mut scaled = Vec::new();
    let mut denoiser = Denoiser::new();
    let mut denoising = false;
    while !stop.load(Ordering::Relaxed) {
        match mic.read() {
            Ok(frame) => {
                scaled.clear();
                scaled.extend_from_slice(frame);
                let suppress = shared.suppress_noise.load(Ordering::Relaxed);
                if suppress && !denoising {
                    // Turned back on: start clean rather than from sound minutes old.
                    denoiser.reset();
                }
                denoising = suppress;
                if suppress {
                    denoiser.process(&mut scaled);
                }
                crate::volume::scale(&mut scaled, shared.mic_gain.get());
                if let Some(taps) = lock(&shared.taps).as_ref() {
                    let _ = taps.audio.try_send(scaled.clone());
                }
                shared
                    .level
                    .store(meter(&scaled).to_bits(), Ordering::Relaxed);
                // Every other 20 ms frame is smooth enough for a meter.
                frames = frames.wrapping_add(1);
                if frames.is_multiple_of(2) {
                    ctx.request_repaint();
                }
            }
            Err(e) => {
                *lock(&shared.mic_error) = Some(e.to_string());
                break;
            }
        }
    }
    shared.level.store(0, Ordering::Relaxed);
}
