//! The microphone thread: reads 20 ms of sound at a time and hands it on.
//!
//! Purpose: each frame goes through noise suppression ([`Denoiser`], while
//! `Shared::suppress_noise` is on), then automatic volume ([`AutoGain`], while
//! `Shared::auto_gain` is on), then the microphone volume, in that order, so the model hears
//! the microphone as it is. The result feeds the level meter and, during a call, the engine's
//! audio tap.
//!
//! Invariants:
//! - The noise model runs while either is on: automatic volume learns only from frames it
//!   scores as speech. With suppression off it scores a copy, and the frame is not changed.
//! - Starting the model again after both were off resets it, so it starts from current sound.
//!   The automatic volume keeps what it learnt until the microphone is reopened.
//! - A device error is kept as text for the interface and the thread ends.
//! - The tap never blocks capture: a full engine queue drops the frame.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use veil_media::{AutoGain, Denoiser};

use super::{Shared, lock, meter, which};

/// Noise suppression and automatic volume for one microphone.
struct Chain {
    denoiser: Denoiser,
    agc: AutoGain,
    /// Whether the noise model ran on the last frame.
    running: bool,
    /// A copy for the model to score when suppression is off; reused.
    probe: Vec<f32>,
}

impl Chain {
    fn new() -> Self {
        Self {
            denoiser: Denoiser::new(),
            agc: AutoGain::new(),
            running: false,
            probe: Vec::new(),
        }
    }

    /// `after` is the mic volume applied next, so Auto volume leaves it room.
    fn apply(&mut self, frame: &mut [f32], suppress: bool, auto: bool, after: f32) {
        let run = suppress || auto;
        if run && !self.running {
            self.denoiser.reset();
        }
        self.running = run;
        let voice = if suppress {
            self.denoiser.process(frame);
            self.denoiser.voice()
        } else if auto {
            self.probe.clear();
            self.probe.extend_from_slice(frame);
            self.denoiser.process(&mut self.probe);
            self.denoiser.voice()
        } else {
            0.0
        };
        if auto {
            self.agc.set_after(after);
            self.agc.process(frame, voice);
        }
    }
}

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
    // The frame after the chain and the volume; reused so they cost no allocation.
    let mut scaled = Vec::new();
    let mut chain = Chain::new();
    while !stop.load(Ordering::Relaxed) {
        #[cfg(target_os = "android")]
        if !veil_media::ffi::android::microphone_allowed() {
            // No call was started: release the native stream even when the UI event loop
            // is paused. Keep only the worker/selection so foreground preview can resume.
            drop(mic);
            shared.level.store(0, Ordering::Relaxed);
            while !stop.load(Ordering::Relaxed) && !veil_media::ffi::android::microphone_allowed() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
            mic = match Microphone::open(&AudioSettings::voice(), id) {
                Ok(mic) => mic,
                Err(error) => {
                    *lock(&shared.mic_error) = Some(error.to_string());
                    ctx.request_repaint();
                    return;
                }
            };
            chain = Chain::new();
        }
        match mic.read() {
            Ok(frame) => {
                *lock(&shared.microphone_at) = Some(std::time::Instant::now());
                scaled.clear();
                scaled.extend_from_slice(frame);
                let mic_gain = shared.mic_gain.get();
                chain.apply(
                    &mut scaled,
                    shared.suppress_noise.load(Ordering::Relaxed),
                    shared.auto_gain.load(Ordering::Relaxed),
                    mic_gain,
                );
                crate::volume::scale(&mut scaled, mic_gain);
                if let Some(taps) = lock(&shared.taps).as_ref() {
                    let _ = taps.audio.try_send(scaled.clone());
                }
                shared
                    .level
                    .store(meter(&scaled).to_bits(), Ordering::Relaxed);
                frames = frames.wrapping_add(1);
                // Every other 20 ms frame is smooth enough for a meter.
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
