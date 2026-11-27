//! The microphone thread: reads 20 ms of sound at a time and hands it on.
//!
//! Purpose: each frame goes through echo cancelling ([`EchoCanceller`], in a call while
//! `Shared::cancel_echo` is on), then noise suppression ([`Denoiser`], while
//! `Shared::suppress_noise` is on), then automatic volume ([`AutoGain`], while
//! `Shared::auto_gain` is on), then the microphone volume, in that order, so the model hears
//! the microphone as it is. The result feeds the level meter and, during a call, the engine's
//! audio tap.
//!
//! Invariants:
//! - Echo cancelling is first, so the noise model hears the microphone with the speakers
//!   already taken out. It is fed what the speaker played ([`Playback::take_reference`]). Out
//!   of a call, or with the switch off, the queue is emptied and the frame left alone.
//! - It starts again (forgets the echo path) when the microphone is reopened, when a speaker
//!   is opened, and when it goes from off to on.
//! - The noise model runs while either is on: automatic volume learns only from frames it
//!   scores as speech. With suppression off it scores a copy, and the frame is not changed.
//! - Starting the model again after both were off resets it, so it starts from current sound.
//!   The automatic volume keeps what it learnt until the microphone is reopened.
//! - A device error is kept as text for the interface and the thread ends.
//! - The tap never blocks capture: a full engine queue drops the frame.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use std::collections::VecDeque;

use veil_media::{AutoGain, Denoiser, EchoCanceller};

use super::{Shared, lock, meter, which};
use crate::volume::Playback;

/// Noise suppression and automatic volume for one microphone.
struct Chain {
    echo: EchoCanceller,
    /// Whether the canceller ran on the last frame.
    cancelling: bool,
    /// `Playback::opens` when the canceller last ran.
    opens: u64,
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
            echo: EchoCanceller::new(),
            cancelling: false,
            opens: 0,
            denoiser: Denoiser::new(),
            agc: AutoGain::new(),
            running: false,
            probe: Vec::new(),
        }
    }

    /// Removes echo from `frame`. `played` is what the speaker took since the last frame;
    /// `opens` is `Playback::opens`.
    fn cancel_echo(&mut self, frame: &mut [f32], on: bool, opens: u64, played: VecDeque<Vec<f32>>) {
        if !on {
            self.cancelling = false;
            return;
        }
        if !self.cancelling || opens != self.opens {
            self.echo.reset();
            self.cancelling = true;
            self.opens = opens;
            // What was queued belongs to the old state.
            return;
        }
        for chunk in &played {
            self.echo.played(chunk);
        }
        self.echo.clean(frame);
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

pub(super) fn mic_loop(
    shared: &Shared,
    playback: &Playback,
    ctx: &egui::Context,
    stop: &AtomicBool,
    id: Option<&str>,
) {
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
                let in_call = lock(&shared.taps).is_some();
                // Always emptied, so it holds nothing stale when cancelling starts.
                let played = playback.take_reference();
                chain.cancel_echo(
                    &mut scaled,
                    in_call && shared.cancel_echo.load(Ordering::Relaxed),
                    playback.opens(),
                    played,
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_cancelling_off_leaves_the_frame() {
        let mut chain = Chain::new();
        let mut frame: Vec<f32> = (0..960).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let before = frame.clone();
        let played = VecDeque::from([frame.clone()]);
        chain.cancel_echo(&mut frame, false, 0, played);
        assert_eq!(frame, before);
    }

    #[test]
    fn echo_cancelling_starts_again_when_switched_on_or_the_speaker_reopens() {
        let mut chain = Chain::new();
        let mut frame = vec![0.1f32; 960];
        chain.cancel_echo(&mut frame, true, 0, VecDeque::new());
        assert!(chain.cancelling);
        chain.cancel_echo(&mut frame, false, 0, VecDeque::new());
        assert!(!chain.cancelling);
        chain.cancel_echo(&mut frame, true, 3, VecDeque::new());
        assert_eq!(chain.opens, 3);
    }

    #[test]
    fn echo_cancelling_removes_what_the_speaker_played() {
        const FRAME: usize = 960;
        let delay = 48 * 60;
        let mut chain = Chain::new();
        let mut seed = 1u32;
        let mut line = vec![0.0f32; delay];
        let frames = 4 * 48_000 / FRAME;
        let tail = frames - 48_000 / FRAME;
        let (mut before, mut after) = (0.0f32, 0.0f32);
        for n in 0..frames {
            // Noise with a slow syllable-like swell, from a fixed seed.
            let played: Vec<f32> = (0..FRAME)
                .map(|i| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    let t = (n * FRAME + i) as f32 / 48_000.0;
                    noise * 0.3 * (0.6 + 0.4 * (t * 4.0 * std::f32::consts::TAU).sin())
                })
                .collect();
            line.extend_from_slice(&played);
            let mut mic: Vec<f32> = line.drain(..FRAME).map(|e| 0.3 * e).collect();
            if n >= tail {
                before += mic.iter().map(|s| s * s).sum::<f32>();
            }
            chain.cancel_echo(&mut mic, true, 0, VecDeque::from([played]));
            if n >= tail {
                after += mic.iter().map(|s| s * s).sum::<f32>();
            }
        }
        let drop = 10.0 * (before / after).log10();
        eprintln!("echo drop {drop:.1} dB");
        assert!(drop >= 20.0, "echo fell only {drop:.1} dB");
    }
}
