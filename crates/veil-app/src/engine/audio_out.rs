//! Everyone else's sound: a thread that orders, decodes, mixes and plays it.
//!
//! Purpose: the session task hands over each opened audio packet ([`AudioIn`]). Per person a
//! [`JitterBuffer`] puts packets back in order and an [`OpusDecoder`] decodes, rebuilds a lost
//! frame from the next packet's FEC, or conceals it. Every 20 ms one frame from each person is
//! summed and written to the speaker, whose queue paces this loop. Each person's level goes to
//! the interface for their speaking ring.
//!
//! Invariants:
//! - The speaker is opened and used on this thread only. If it fails (a headset unplugged),
//!   sound is dropped and the speaker is opened again after two seconds.
//! - A different speaker chosen during the call ([`Playback`]) is opened on the next frame;
//!   the speaker volume applies to the mix each frame.
//! - The mix is clamped to -1..1, so several loud people clip rather than wrap.
//! - Someone silent for 10 s is forgotten, decoder and all.
//! - With "Remove noise from others" on ([`Playback::clean_others`]), each person's decoded
//!   sound goes through their own [`Denoiser`] before the mix and their level. Turning it off
//!   drops the denoisers, so turning it on again starts each one fresh.
//!
//! - Computer sound from a share is a source of its own per sharer, decoded in stereo, mixed
//!   down to the speaker's mono and played at that share's level here (`ShareLevel`). It is
//!   played only while that person's signed screen share is showing, so a member cannot slip
//!   sound in as someone else's share. It never lights a speaking ring.
//!
//! shortcut: share sound is played in mono, as the speaker is opened for the call's mono
//! voice. Open the speaker in stereo once stereo matters to someone.
//!
//! shortcut: no echo cancellation of Veil's own (Windows applies the device's where it has
//! one, see `WasapiPlayback`). Two copies on one computer need headphones.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use veil_media::{
    AudioHeader, AudioSettings, Denoiser, JitterBuffer, MediaError, OpusDecoder, Playout,
};

use super::view::{SPEAKING, Shared};
use crate::media::{Worker, meter};
use crate::volume::{Playback, scale};

const REOPEN_AFTER: Duration = Duration::from_secs(2);
const FORGET_AFTER: Duration = Duration::from_secs(10);

/// One opened audio packet from participant `id`, stamped on the session's clock.
pub struct AudioIn {
    pub id: u32,
    pub header: AudioHeader,
    pub packet: Vec<u8>,
    pub arrival_us: u64,
}

pub fn spawn(packets: Receiver<AudioIn>, shared: Arc<Shared>, playback: Arc<Playback>) -> Worker {
    Worker::spawn(move |stop| run(&packets, &shared, playback, &stop))
}

struct Source {
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    /// Made when noise removal for others is first on, dropped when it is turned off.
    denoiser: Option<Denoiser>,
    last: Instant,
}

/// Adds `frame` into `mix`; a frame of the wrong length is ignored.
pub fn mix_into(mix: &mut [f32], frame: &[f32]) {
    if frame.len() != mix.len() {
        return;
    }
    for (out, sample) in mix.iter_mut().zip(frame) {
        *out += sample;
    }
}

fn run(packets: &Receiver<AudioIn>, shared: &Shared, playback: Arc<Playback>, stop: &AtomicBool) {
    let settings = AudioSettings::voice();
    let per_channel = settings.frame_samples();
    let mut mix = vec![0.0f32; per_channel * settings.channels.count()];
    // By participant id, and whether it is their share's computer sound.
    let mut sources: HashMap<(u32, bool), Source> = HashMap::new();
    // One share's sound mixed down to mono; reused.
    let mut mono = Vec::with_capacity(per_channel);
    let mut speaker_out = Speaker::new(settings, playback);
    // One person's frame after noise removal; reused so it costs no allocation.
    let mut cleaned = Vec::with_capacity(mix.len());
    while !stop.load(Ordering::Relaxed) {
        loop {
            match packets.try_recv() {
                Ok(packet) => receive(&mut sources, packet, &settings, shared),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        mix.fill(0.0);
        let clean = speaker_out.playback.clean_others.load(Ordering::Relaxed);
        let sharers = shared.sharers();
        for (&(id, share), source) in &mut sources {
            let frame = match source.jitter.pop() {
                Playout::Packet(packet) => source.decoder.decode(&packet),
                Playout::Recover(next) => source.decoder.recover(&next, per_channel),
                Playout::Conceal => source.decoder.conceal(per_channel),
                Playout::Wait => Ok(&[][..]),
            };
            if share {
                let gain = speaker_out.playback.share_level(id).gain();
                if let Ok(frame) = frame
                    && !frame.is_empty()
                    && gain > 0.0
                    && sharers.contains(&id)
                {
                    downmix(frame, &mut mono);
                    scale(&mut mono, gain);
                    mix_into(&mut mix, &mono);
                }
                continue;
            }
            let level = match frame {
                Ok(frame) if !frame.is_empty() => {
                    let heard = if clean {
                        let denoiser = source.denoiser.get_or_insert_with(Denoiser::new);
                        cleaned.clear();
                        cleaned.extend_from_slice(frame);
                        denoiser.process(&mut cleaned);
                        &cleaned[..]
                    } else {
                        source.denoiser = None;
                        frame
                    };
                    mix_into(&mut mix, heard);
                    meter(heard)
                }
                _ => 0.0,
            };
            shared.heard(id, |heard| {
                heard.level = level;
                if level > SPEAKING {
                    heard.loud_at = Some(Instant::now());
                }
            });
        }
        scale(&mut mix, speaker_out.playback.volume.get());
        speaker_out.play(&mix);
        let now = Instant::now();
        sources.retain(|&(id, share), source| {
            let keep = now.saturating_duration_since(source.last) < FORGET_AFTER;
            if !keep && share {
                shared.forget_share_heard(id);
            } else if !keep {
                shared.forget_heard(id);
            }
            keep
        });
    }
}

/// Mixes interleaved stereo down to mono in `out`.
pub fn downmix(stereo: &[f32], out: &mut Vec<f32>) {
    out.clear();
    out.extend(stereo.as_chunks::<2>().0.iter().map(|[l, r]| (l + r) * 0.5));
}

fn receive(
    sources: &mut HashMap<(u32, bool), Source>,
    packet: AudioIn,
    voice: &AudioSettings,
    shared: &Shared,
) {
    let now = Instant::now();
    let share = packet.header.share;
    let settings = if share {
        AudioSettings::share_sound()
    } else {
        *voice
    };
    let source = match sources.entry((packet.id, share)) {
        Entry::Occupied(source) => source.into_mut(),
        Entry::Vacant(slot) => match OpusDecoder::new(&settings) {
            Ok(decoder) => slot.insert(Source {
                jitter: JitterBuffer::new(settings.frame_ms),
                decoder,
                denoiser: None,
                last: now,
            }),
            Err(e) => {
                crate::note!("no audio decoder: {e}");
                return;
            }
        },
    };
    source
        .jitter
        .push(packet.header, &packet.packet, packet.arrival_us);
    source.last = now;
    if share {
        shared.share_heard(packet.id);
    } else {
        shared.heard(packet.id, |heard| heard.at = now);
    }
}

/// The speaker, opened on first use, again after it fails, and when another is chosen.
struct Speaker {
    settings: AudioSettings,
    playback: Arc<Playback>,
    /// `playback.changes()` when the open speaker was chosen.
    seen: u64,
    open: Option<Output>,
    retry_at: Instant,
}

impl Speaker {
    fn new(settings: AudioSettings, playback: Arc<Playback>) -> Self {
        Self {
            settings,
            seen: playback.changes(),
            playback,
            open: None,
            retry_at: Instant::now(),
        }
    }

    /// Plays one frame. Paces the caller to real time while the speaker works; without one it
    /// sleeps a frame's length instead.
    fn play(&mut self, frame: &[f32]) {
        let changes = self.playback.changes();
        if changes != self.seen {
            // Another speaker was chosen: close this one and open that one now.
            self.seen = changes;
            self.open = None;
            self.retry_at = Instant::now();
        }
        if self.open.is_none() && Instant::now() >= self.retry_at {
            let device = self.playback.device();
            match Output::open(&self.settings, device.as_deref()) {
                Ok(output) => {
                    let which = crate::media::which(device.as_deref());
                    crate::note!("speaker: opened {which}");
                    self.open = Some(output);
                }
                Err(e) => {
                    let which = crate::media::which(device.as_deref());
                    crate::note!("speaker: cannot open {which}: {e}");
                    self.retry_at = Instant::now() + REOPEN_AFTER;
                }
            }
        }
        let failed = match &mut self.open {
            Some(output) => output.write(frame).is_err(),
            None => {
                std::thread::sleep(Duration::from_millis(u64::from(self.settings.frame_ms)));
                false
            }
        };
        if failed {
            self.open = None;
            self.retry_at = Instant::now() + REOPEN_AFTER;
        }
    }
}

struct Output(veil_media::platform::Speaker);

impl Output {
    fn open(settings: &AudioSettings, device: Option<&str>) -> Result<Self, MediaError> {
        veil_media::platform::Speaker::open(settings, device).map(Self)
    }

    fn write(&mut self, frame: &[f32]) -> Result<(), MediaError> {
        self.0.write(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_adds_and_clamps() {
        let mut mix = vec![0.0f32; 4];
        mix_into(&mut mix, &[0.5, -0.5, 0.75, 0.0]);
        mix_into(&mut mix, &[0.75, -0.75, 0.0, 0.25]);
        mix_into(&mut mix, &[1.0; 3]);
        scale(&mut mix, 1.0);
        assert_eq!(mix, [1.0, -1.0, 0.75, 0.25]);
    }

    #[test]
    fn share_sound_is_mixed_down_to_mono() {
        let mut mono = vec![9.0];
        downmix(&[1.0, 0.0, -0.5, -0.5, 0.25], &mut mono);
        assert_eq!(mono, [0.5, -0.5], "a stray sample is dropped");
    }
}
