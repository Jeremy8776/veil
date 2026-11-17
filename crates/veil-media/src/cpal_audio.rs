//! Microphone and speakers on Linux (ALSA) and macOS (Core Audio), through cpal.
//!
//! Purpose: the same shape as the Windows WASAPI types, so the app treats them alike.
//! [`CpalCapture::read`] blocks for the next frame of `frame_samples` per channel, and
//! [`CpalPlayback::write`] queues a frame and waits while two are already queued, which paces
//! the caller to the device's clock.
//!
//! Invariants:
//! - Always 48 kHz in the call's channel count. A device that cannot run at 48 kHz is refused
//!   ([`MediaError::Unavailable`]) rather than resampled. Channels are mixed with
//!   [`crate::remix`].
//! - cpal calls back on its own thread. The callbacks only move samples through a bounded
//!   channel or a short-held lock; they never block on the call engine.
//! - A device that stops delivering or taking audio for [`STALL`] is an error, so the caller
//!   can reopen it, as on Windows.
//! - No `unsafe` here; cpal's own native code is listed in docs/DEPENDENCIES.md.
//!
//! shortcut: no resampling, so a device without 48 kHz is unusable. ALSA's `default` device
//! and Core Audio both resample for us in practice; add a resampler if reports show otherwise.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig};

use crate::MediaError;
use crate::audio::{AudioSettings, SAMPLE_RATE};
use crate::remix::remix;

/// The longest a device may go without delivering or taking audio.
const STALL: Duration = Duration::from_secs(2);
/// Device callbacks held for a slow reader, about a second's worth at typical sizes.
const QUEUE_CALLBACKS: usize = 64;
/// Frames queued for the speaker before `write` waits.
const QUEUED_FRAMES: usize = 2;

/// A microphone or speaker: its name for people, its id for opening it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub id: String,
}

fn list(devices: Result<impl Iterator<Item = Device>, cpal::Error>) -> Result<Vec<AudioDevice>, MediaError> {
    let devices = devices.map_err(|_| MediaError::Unavailable("audio devices"))?;
    Ok(devices
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            let name = device
                .description()
                .map(|d| d.name().to_owned())
                .unwrap_or_else(|_| id.clone());
            Some(AudioDevice { name, id })
        })
        .collect())
}

/// Every microphone.
pub fn microphones() -> Result<Vec<AudioDevice>, MediaError> {
    list(cpal::default_host().input_devices())
}

/// Every speaker or headset output.
pub fn speakers() -> Result<Vec<AudioDevice>, MediaError> {
    list(cpal::default_host().output_devices())
}

/// The device with this id, or the default one for `None`.
fn find(id: Option<&str>, input: bool) -> Result<Device, MediaError> {
    let host = cpal::default_host();
    let device = match id {
        Some(id) => id
            .parse()
            .ok()
            .and_then(|id| host.device_by_id(&id)),
        None if input => host.default_input_device(),
        None => host.default_output_device(),
    };
    device.ok_or(MediaError::Unavailable(if input { "microphone" } else { "speaker" }))
}

/// A 48 kHz configuration in f32 or i16 (f32 preferred), with the fewest channels that still
/// covers the call's, else the most the device has.
fn pick(
    configs: Result<impl Iterator<Item = cpal::SupportedStreamConfigRange>, cpal::Error>,
    wanted: usize,
) -> Result<SupportedStreamConfig, MediaError> {
    let configs = configs.map_err(|_| MediaError::Unavailable("audio formats"))?;
    configs
        .filter(|c| matches!(c.sample_format(), SampleFormat::F32 | SampleFormat::I16))
        .filter_map(|c| c.try_with_sample_rate(SAMPLE_RATE))
        .min_by_key(|c| {
            let channels = usize::from(c.channels());
            let covers = channels >= wanted;
            (
                !covers,
                if covers { channels } else { usize::MAX - channels },
                c.sample_format() != SampleFormat::F32,
            )
        })
        .ok_or(MediaError::Unavailable("48 kHz audio"))
}

/// Whether a stream error means the device is gone. An xrun is a glitch, a rerouted default
/// device keeps playing, and a refused real-time priority only risks glitches; none of them
/// should end a call's audio.
fn is_fatal(error: &cpal::Error) -> bool {
    !matches!(
        error.kind(),
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An open microphone.
pub struct CpalCapture {
    _stream: Stream,
    samples: Receiver<Vec<f32>>,
    failed: Arc<Mutex<bool>>,
    frame_len: usize,
    pending: Vec<f32>,
    frame: Vec<f32>,
}

impl CpalCapture {
    /// Opens and starts a microphone: `device` is an id from [`microphones`], or `None` for
    /// the default. [`MediaError::Unavailable`] means there is no such device.
    pub fn open(settings: &AudioSettings, device: Option<&str>) -> Result<Self, MediaError> {
        settings.check()?;
        let device = find(device, true)?;
        let channels = settings.channels.count();
        let supported = pick(device.supported_input_configs(), channels)?;
        let config = supported.config();
        let (tx, rx) = sync_channel(QUEUE_CALLBACKS);
        let failed = Arc::new(Mutex::new(false));
        let stream = match supported.sample_format() {
            SampleFormat::I16 => input::<i16>(&device, &config, channels, tx, failed.clone())?,
            _ => input::<f32>(&device, &config, channels, tx, failed.clone())?,
        };
        stream.play().map_err(|_| MediaError::Capture("start"))?;
        Ok(Self {
            _stream: stream,
            samples: rx,
            failed,
            frame_len: settings.frame_samples() * channels,
            pending: Vec::new(),
            frame: Vec::new(),
        })
    }

    /// Waits for the next frame: `frame_samples` per channel, interleaved.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        let deadline = Instant::now() + STALL;
        while self.pending.len() < self.frame_len {
            if *lock(&self.failed) {
                return Err(MediaError::Capture("device lost"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.samples.recv_timeout(left) {
                Ok(chunk) => self.pending.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => return Err(MediaError::Capture("microphone stalled")),
                Err(RecvTimeoutError::Disconnected) => return Err(MediaError::Capture("device lost")),
            }
        }
        self.frame.clear();
        self.frame.extend(self.pending.drain(..self.frame_len));
        Ok(&self.frame)
    }
}

fn input<T>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    tx: SyncSender<Vec<f32>>,
    failed: Arc<Mutex<bool>>,
) -> Result<Stream, MediaError>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let from = usize::from(config.channels);
    let on_error = {
        let failed = failed.clone();
        move |error: cpal::Error| {
            if is_fatal(&error) {
                *lock(&failed) = true;
            }
        }
    };
    let mut floats = Vec::new();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                floats.clear();
                floats.extend(data.iter().map(|s| s.to_sample::<f32>()));
                let mut out = Vec::with_capacity(floats.len() / from.max(1) * channels);
                remix(&floats, from, channels, &mut out);
                // A full queue means the reader is behind: drop this chunk, never block.
                if let Err(TrySendError::Disconnected(_)) = tx.try_send(out) {
                    *lock(&failed) = true;
                }
            },
            on_error,
            None,
        )
        .map_err(|_| MediaError::Unavailable("microphone format"))
}

/// What the output callback and `write` share.
#[derive(Default)]
struct Queue {
    samples: VecDeque<f32>,
    /// Set by the callback each time it takes audio, for stall detection.
    last_pull: Option<Instant>,
    failed: bool,
}

/// An open speaker. Playback starts with the first frame written.
pub struct CpalPlayback {
    stream: Stream,
    queue: Arc<Mutex<Queue>>,
    frame_len: usize,
    playing: bool,
}

impl CpalPlayback {
    /// Opens a speaker: `device` is an id from [`speakers`], or `None` for the default.
    pub fn open(settings: &AudioSettings, device: Option<&str>) -> Result<Self, MediaError> {
        settings.check()?;
        let device = find(device, false)?;
        let channels = settings.channels.count();
        let supported = pick(device.supported_output_configs(), channels)?;
        let config = supported.config();
        let queue = Arc::new(Mutex::new(Queue::default()));
        let stream = match supported.sample_format() {
            SampleFormat::I16 => output::<i16>(&device, &config, channels, queue.clone())?,
            _ => output::<f32>(&device, &config, channels, queue.clone())?,
        };
        Ok(Self {
            stream,
            queue,
            frame_len: settings.frame_samples() * channels,
            playing: false,
        })
    }

    /// Queues one frame (`frame_samples` per channel, interleaved), waiting while two are
    /// already queued. Calling this in a loop plays in step with the device's clock.
    pub fn write(&mut self, pcm: &[f32]) -> Result<(), MediaError> {
        if pcm.len() != self.frame_len {
            return Err(MediaError::Malformed("audio frame length"));
        }
        lock(&self.queue).samples.extend(pcm.iter().copied());
        if !self.playing {
            self.stream.play().map_err(|_| MediaError::Playback("start"))?;
            self.playing = true;
            lock(&self.queue).last_pull = Some(Instant::now());
        }
        loop {
            let (queued, last_pull, failed) = {
                let queue = lock(&self.queue);
                (queue.samples.len(), queue.last_pull, queue.failed)
            };
            if failed {
                return Err(MediaError::Playback("device lost"));
            }
            if queued <= self.frame_len * QUEUED_FRAMES {
                return Ok(());
            }
            if last_pull.is_some_and(|t| t.elapsed() > STALL) {
                return Err(MediaError::Playback("speaker stalled"));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn output<T>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    queue: Arc<Mutex<Queue>>,
) -> Result<Stream, MediaError>
where
    T: SizedSample + cpal::FromSample<f32>,
{
    let to = usize::from(config.channels);
    let on_error = {
        let queue = queue.clone();
        move |error: cpal::Error| {
            if is_fatal(&error) {
                lock(&queue).failed = true;
            }
        }
    };
    let (mut call, mut device_samples) = (Vec::new(), Vec::new());
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let wanted = data.len() / to.max(1) * channels;
                call.clear();
                {
                    let mut queue = lock(&queue);
                    queue.last_pull = Some(Instant::now());
                    let take = wanted.min(queue.samples.len());
                    call.extend(queue.samples.drain(..take));
                }
                // Underrun: silence rather than a click of old audio.
                call.resize(wanted, 0.0);
                device_samples.clear();
                remix(&call, channels, to, &mut device_samples);
                for (out, sample) in data.iter_mut().zip(device_samples.iter().chain(std::iter::repeat(&0.0))) {
                    *out = T::from_sample(*sample);
                }
            },
            on_error,
            None,
        )
        .map_err(|_| MediaError::Unavailable("speaker format"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::ErrorKind;

    #[test]
    fn glitches_and_reroutes_are_not_fatal() {
        for kind in [ErrorKind::Xrun, ErrorKind::DeviceChanged, ErrorKind::RealtimeDenied] {
            assert!(!is_fatal(&cpal::Error::new(kind)), "{kind:?}");
        }
    }

    #[test]
    fn losing_the_device_is_fatal() {
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::BackendError,
            ErrorKind::Other,
        ] {
            assert!(is_fatal(&cpal::Error::new(kind)), "{kind:?}");
        }
    }
}
