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
//!
//! shortcut: no echo cancellation of Veil's own (Windows applies the device's where it has
//! one, see `WasapiPlayback`). Two copies on one computer need headphones.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use veil_media::{AudioHeader, AudioSettings, JitterBuffer, MediaError, OpusDecoder, Playout};

use super::view::Shared;
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
    let mut sources: HashMap<u32, Source> = HashMap::new();
    let mut speaker_out = Speaker::new(settings, playback);
    while !stop.load(Ordering::Relaxed) {
        loop {
            match packets.try_recv() {
                Ok(packet) => receive(&mut sources, packet, &settings, shared),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        mix.fill(0.0);
        for (id, source) in &mut sources {
            let frame = match source.jitter.pop() {
                Playout::Packet(packet) => source.decoder.decode(&packet),
                Playout::Recover(next) => source.decoder.recover(&next, per_channel),
                Playout::Conceal => source.decoder.conceal(per_channel),
                Playout::Wait => Ok(&[][..]),
            };
            let level = match frame {
                Ok(frame) if !frame.is_empty() => {
                    mix_into(&mut mix, frame);
                    meter(frame)
                }
                _ => 0.0,
            };
            shared.heard(*id, |heard| heard.level = level);
        }
        scale(&mut mix, speaker_out.playback.volume.get());
        speaker_out.play(&mix);
        let now = Instant::now();
        sources.retain(|id, source| {
            let keep = now.saturating_duration_since(source.last) < FORGET_AFTER;
            if !keep {
                shared.forget_heard(*id);
            }
            keep
        });
    }
}

fn receive(
    sources: &mut HashMap<u32, Source>,
    packet: AudioIn,
    settings: &AudioSettings,
    shared: &Shared,
) {
    let now = Instant::now();
    let source = match sources.entry(packet.id) {
        Entry::Occupied(source) => source.into_mut(),
        Entry::Vacant(slot) => match OpusDecoder::new(settings) {
            Ok(decoder) => slot.insert(Source {
                jitter: JitterBuffer::new(settings.frame_ms),
                decoder,
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
    shared.heard(packet.id, |heard| heard.at = now);
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
                Ok(output) => self.open = Some(output),
                Err(e) => {
                    crate::note!("cannot open the speaker: {e}");
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
}
