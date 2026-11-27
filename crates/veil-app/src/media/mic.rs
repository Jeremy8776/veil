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
//! - On Windows the microphone opens without the driver's voice effects while the switch is on,
//!   so echo is not cancelled twice, and with them while it is off; flipping the switch
//!   reopens it. A device that refuses is logged and keeps its effects.
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

/// Opens the microphone. On Windows `raw` asks for it without the driver's voice effects.
fn open_mic(id: Option<&str>, raw: bool) -> Result<veil_media::platform::Microphone, String> {
    use veil_media::AudioSettings;
    use veil_media::platform::Microphone;
    #[cfg(windows)]
    {
        let mic =
            Microphone::open_with(&AudioSettings::voice(), id, raw).map_err(|e| e.to_string())?;
        if let Some(step) = mic.raw_refused() {
            crate::note!("microphone: no raw mode ({step}), the driver's voice effects stay on");
        }
        Ok(mic)
    }
    #[cfg(not(windows))]
    {
        let _ = raw;
        Microphone::open(&AudioSettings::voice(), id).map_err(|e| e.to_string())
    }
}

pub(super) fn mic_loop(
    shared: &Shared,
    playback: &Playback,
    ctx: &egui::Context,
    stop: &AtomicBool,
    id: Option<&str>,
) {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut raw = shared.cancel_echo.load(Ordering::Relaxed);
    let mut mic = match open_mic(id, raw) {
        Ok(mic) => {
            crate::note!("microphone: opened {}", which(id));
            mic
        }
        Err(e) => {
            crate::note!("microphone: cannot open {}: {e}", which(id));
            *lock(&shared.mic_error) = Some(e);
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
            mic = match open_mic(id, raw) {
                Ok(mic) => mic,
                Err(error) => {
                    *lock(&shared.mic_error) = Some(error);
                    ctx.request_repaint();
                    return;
                }
            };
            chain = Chain::new();
        }
        #[cfg(windows)]
        if shared.cancel_echo.load(Ordering::Relaxed) != raw {
            raw = !raw;
            drop(mic);
            mic = match open_mic(id, raw) {
                Ok(mic) => mic,
                Err(e) => {
                    crate::note!("microphone: cannot reopen {}: {e}", which(id));
                    *lock(&shared.mic_error) = Some(e);
                    ctx.request_repaint();
                    return;
                }
            };
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

    /// A speaker playing speech-like noise (fixed seed) into a microphone that hears it 60 ms
    /// later at 0.3.
    #[derive(Clone)]
    struct Room {
        seed: u32,
        n: usize,
        line: Vec<f32>,
    }

    impl Room {
        const FRAME: usize = 960;

        fn new() -> Self {
            Self {
                seed: 1,
                n: 0,
                line: vec![0.0; 48 * 60],
            }
        }

        /// Plays one frame and runs the microphone's frame through `chain`: returns the
        /// microphone's energy before, and the frame after.
        fn frame(&mut self, chain: &mut Chain, opens: u64) -> (f32, Vec<f32>) {
            let played: Vec<f32> = (0..Self::FRAME)
                .map(|i| {
                    self.seed ^= self.seed << 13;
                    self.seed ^= self.seed >> 17;
                    self.seed ^= self.seed << 5;
                    let noise = self.seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    let t = (self.n * Self::FRAME + i) as f32 / 48_000.0;
                    noise * 0.3 * (0.6 + 0.4 * (t * 4.0 * std::f32::consts::TAU).sin())
                })
                .collect();
            self.n += 1;
            self.line.extend_from_slice(&played);
            let mut mic: Vec<f32> = self.line.drain(..Self::FRAME).map(|e| 0.3 * e).collect();
            let before = mic.iter().map(|s| s * s).sum::<f32>();
            chain.cancel_echo(&mut mic, true, opens, VecDeque::from([played]));
            (before, mic)
        }

        /// Runs `frames` frames and returns how far the echo fell, in dB.
        fn drop(&mut self, chain: &mut Chain, opens: u64, frames: usize) -> f32 {
            let (mut before, mut after) = (0.0, 0.0);
            for _ in 0..frames {
                let (b, a) = self.frame(chain, opens);
                before += b;
                after += a.iter().map(|s| s * s).sum::<f32>();
            }
            10.0 * (before / after).log10()
        }
    }

    #[test]
    fn echo_cancelling_starts_again_when_switched_on_or_the_speaker_reopens() {
        let mut chain = Chain::new();
        let mut frame = vec![0.1f32; 960];
        chain.cancel_echo(&mut frame, true, 0, VecDeque::new());
        assert!(chain.cancelling);
        chain.cancel_echo(&mut frame, false, 0, VecDeque::new());
        assert!(!chain.cancelling);
        // Learn the echo path, then open another speaker: from then on the chain must behave
        // exactly as a new one hearing the same sound.
        let mut room = Room::new();
        assert!(room.drop(&mut chain, 0, 200) >= 20.0);
        let mut same_room = room.clone();
        let mut new_chain = Chain::new();
        for _ in 0..50 {
            assert_eq!(
                room.frame(&mut chain, 3).1,
                same_room.frame(&mut new_chain, 3).1
            );
        }
        assert_eq!(chain.opens, 3);
    }

    #[test]
    fn echo_cancelling_removes_what_the_speaker_played() {
        let mut chain = Chain::new();
        let mut room = Room::new();
        room.drop(&mut chain, 0, 150);
        let drop = room.drop(&mut chain, 0, 50);
        eprintln!("echo drop {drop:.1} dB");
        assert!(drop >= 20.0, "echo fell only {drop:.1} dB");
    }
}
