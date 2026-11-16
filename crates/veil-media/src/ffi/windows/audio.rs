//! Windows audio through WASAPI: the microphone in, the speakers out, 48 kHz float frames.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: [`WasapiCapture`] reads one Opus frame of microphone audio at a time, and
//! [`WasapiPlayback`] plays one decoded frame at a time, pacing the caller to the device.
//! Both use the default communications device in shared mode, and Windows converts to and
//! from the device's own format (`AUTOCONVERTPCM`), so the rest of Veil sees only 48 kHz
//! float samples in the channel count of [`AudioSettings`].
//!
//! Invariants:
//! - Event driven: a call waits on the device's event, at most [`STALL`], never spins.
//! - Samples are copied byte by byte out of and into the locked device buffer, within the
//!   frame count WASAPI gives, so no alignment is assumed.
//! - Capture holds at most [`MAX_CAPTURE_BACKLOG`] frames; older audio is dropped so a slow
//!   reader does not fall behind for good. Playback queues at most two frames, so its delay
//!   stays near 40 ms at 20 ms frames.
//! - The streams use the communications category, so Windows applies the device's voice
//!   effects (echo cancellation and noise suppression) where its driver offers them.
//! - Field order matters: the client is stopped and released before its event handle is
//!   closed, then COM is shut down.
//!
//! shortcut: the default device only, and a lost device (a headset unplugged) is an error the
//! app must handle by opening again. Add device choice and follow-the-default when the app has
//! a settings screen.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AudioCategory_Communications, AudioClientProperties,
    EDataFlow, IAudioCaptureClient, IAudioClient, IAudioClient2, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0, eCapture, eCommunications, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{GUID, Interface};

use super::Runtime;
use crate::MediaError;
use crate::audio::{AudioSettings, Channels, SAMPLE_RATE};

/// The longest a call waits for the device before treating it as stalled.
const STALL: u32 = 500;
/// The most frames of microphone audio held for a slow reader.
const MAX_CAPTURE_BACKLOG: usize = 5;
/// `WAVE_FORMAT_EXTENSIBLE`, written here to avoid a whole `windows` feature for one number.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`, likewise.
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// Speaker masks: front centre for mono, front left and right for stereo.
const MASK_MONO: u32 = 0x4;
const MASK_STEREO: u32 = 0x3;

/// An auto-reset event the audio engine signals, closed on drop.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed exactly once, after the
        // client that signalled it has been released (field order).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// One initialised audio client and what it was set up with.
struct Endpoint {
    client: IAudioClient,
    /// Samples (all channels) in one frame.
    frame: usize,
    channels: usize,
    buffer_frames: u32,
    fail: fn(&'static str) -> MediaError,
    event: Event,
}

impl Endpoint {
    fn open(
        flow: EDataFlow,
        settings: &AudioSettings,
        fail: fn(&'static str) -> MediaError,
    ) -> Result<Self, MediaError> {
        settings.check()?;
        // SAFETY: the class id is a static GUID and no outer object is passed.
        let devices: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|_| fail("device list"))?;
        // SAFETY: `devices` is live; the flow and role are plain values.
        let device = unsafe { devices.GetDefaultAudioEndpoint(flow, eCommunications) }
            .map_err(|_| MediaError::Unavailable("audio device"))?;
        // SAFETY: `device` is live; no activation parameters are passed.
        let client: IAudioClient =
            unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|_| fail("activate"))?;
        if let Ok(client2) = client.cast::<IAudioClient2>() {
            let properties = AudioClientProperties {
                cbSize: size_of::<AudioClientProperties>() as u32,
                eCategory: AudioCategory_Communications,
                ..Default::default()
            };
            // SAFETY: `properties` is a valid struct of the size it states, alive for the
            // call. Optional: a client without categories still plays.
            let _ = unsafe { client2.SetClientProperties(&properties) };
        }
        let channels = settings.channels.count();
        let format = float_format(settings.channels);
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // Three frames of buffer, in 100 ns units.
        let duration = i64::from(settings.frame_ms) * 3 * 10_000;
        // SAFETY: `format` is a valid WAVEFORMATEXTENSIBLE whose first member is the
        // WAVEFORMATEX the pointer names, with `cbSize` covering the rest; it outlives the call.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                duration,
                0,
                &format.Format,
                None,
            )
        }
        .map_err(|_| fail("audio format"))?;
        // SAFETY: no pointers are passed: an unnamed auto-reset event, not signalled.
        let event =
            Event(unsafe { CreateEventW(None, false, false, None) }.map_err(|_| fail("event"))?);
        // SAFETY: `client` is live and `event` stays open longer than it (field order).
        unsafe { client.SetEventHandle(event.0) }.map_err(|_| fail("event"))?;
        // SAFETY: `client` is initialised.
        let buffer_frames = unsafe { client.GetBufferSize() }.map_err(|_| fail("buffer size"))?;
        Ok(Self {
            client,
            frame: settings.frame_samples() * channels,
            channels,
            buffer_frames,
            fail,
            event,
        })
    }

    /// Waits for the device's next period.
    fn wait(&self) -> Result<(), MediaError> {
        // SAFETY: the event handle is open for as long as `self`.
        if unsafe { WaitForSingleObject(self.event.0, STALL) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err((self.fail)("audio device stalled"))
        }
    }

    fn start(&self) -> Result<(), MediaError> {
        // SAFETY: `client` is initialised; Start takes no pointers.
        unsafe { self.client.Start() }.map_err(|e| self.error(e, "start"))
    }

    fn error(&self, e: windows::core::Error, step: &'static str) -> MediaError {
        if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
            (self.fail)("audio device lost")
        } else {
            (self.fail)(step)
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // SAFETY: `client` is live; Stop takes no pointers and is harmless when stopped.
        let _ = unsafe { self.client.Stop() };
    }
}

/// 48 kHz 32-bit float in the requested channel count.
fn float_format(channels: Channels) -> WAVEFORMATEXTENSIBLE {
    let count = channels.count() as u16;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: count,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 4 * u32::from(count),
            nBlockAlign: 4 * count,
            wBitsPerSample: 32,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: match channels {
            Channels::Mono => MASK_MONO,
            Channels::Stereo => MASK_STEREO,
        },
        SubFormat: SUBTYPE_FLOAT,
    }
}

/// The default communications microphone.
pub struct WasapiCapture {
    capture: IAudioCaptureClient,
    endpoint: Endpoint,
    pending: Vec<f32>,
    out: Vec<f32>,
    _runtime: Runtime,
}

impl WasapiCapture {
    /// Opens and starts the microphone. [`MediaError::Unavailable`] means there is none.
    pub fn open(settings: &AudioSettings) -> Result<Self, MediaError> {
        let runtime = Runtime::start()?;
        let endpoint = Endpoint::open(eCapture, settings, MediaError::Capture)?;
        // SAFETY: the client is initialised for capture.
        let capture: IAudioCaptureClient =
            unsafe { endpoint.client.GetService() }.map_err(|_| MediaError::Capture("service"))?;
        endpoint.start()?;
        Ok(Self {
            capture,
            endpoint,
            pending: Vec::new(),
            out: Vec::new(),
            _runtime: runtime,
        })
    }

    /// Waits for the next frame: `frame_samples` per channel, interleaved.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        let frame = self.endpoint.frame;
        while self.pending.len() < frame {
            // SAFETY: the capture client is live.
            let next = unsafe { self.capture.GetNextPacketSize() }
                .map_err(|e| self.endpoint.error(e, "packet size"))?;
            if next == 0 {
                self.endpoint.wait()?;
                continue;
            }
            self.take_packet()?;
        }
        let backlog = frame * MAX_CAPTURE_BACKLOG;
        if self.pending.len() > backlog {
            self.pending.drain(..self.pending.len() - backlog);
        }
        self.out.clear();
        self.out.extend(self.pending.drain(..frame));
        Ok(&self.out)
    }

    /// Copies one packet from the device buffer into `pending`.
    fn take_packet(&mut self) -> Result<(), MediaError> {
        let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
        // SAFETY: the three are valid out values. On success `data` points to `frames` frames
        // of the format set in Initialize until ReleaseBuffer.
        unsafe {
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
        }
        .map_err(|e| self.endpoint.error(e, "read buffer"))?;
        let samples = frames as usize * self.endpoint.channels;
        if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            self.pending.resize(self.pending.len() + samples, 0.0);
        } else {
            // SAFETY: `data` points to `frames` frames of 4-byte samples while the buffer is
            // held, and the bytes are copied before ReleaseBuffer.
            let bytes = unsafe { std::slice::from_raw_parts(data, samples * 4) };
            self.pending.extend(
                bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        // SAFETY: releases exactly the frames GetBuffer gave.
        unsafe { self.capture.ReleaseBuffer(frames) }
            .map_err(|e| self.endpoint.error(e, "release buffer"))
    }
}

/// The default communications speakers or headset.
pub struct WasapiPlayback {
    render: IAudioRenderClient,
    endpoint: Endpoint,
    started: bool,
    _runtime: Runtime,
}

impl WasapiPlayback {
    /// Opens the speakers. Playback starts with the first frame written.
    pub fn open(settings: &AudioSettings) -> Result<Self, MediaError> {
        let runtime = Runtime::start()?;
        let endpoint = Endpoint::open(eRender, settings, MediaError::Playback)?;
        // SAFETY: the client is initialised for rendering.
        let render: IAudioRenderClient =
            unsafe { endpoint.client.GetService() }.map_err(|_| MediaError::Playback("service"))?;
        Ok(Self {
            render,
            endpoint,
            started: false,
            _runtime: runtime,
        })
    }

    /// Queues one frame (`frame_samples` per channel, interleaved), waiting while two are
    /// already queued. Calling this in a loop plays in step with the device's clock.
    pub fn write(&mut self, pcm: &[f32]) -> Result<(), MediaError> {
        if pcm.len() != self.endpoint.frame {
            return Err(MediaError::Malformed("audio frame length"));
        }
        let frames = (pcm.len() / self.endpoint.channels) as u32;
        let limit = self.endpoint.buffer_frames.min(frames * 2);
        loop {
            // SAFETY: the client is initialised.
            let queued = unsafe { self.endpoint.client.GetCurrentPadding() }
                .map_err(|e| self.endpoint.error(e, "queue"))?;
            if queued + frames <= limit {
                break;
            }
            self.endpoint.wait()?;
        }
        // SAFETY: room for `frames` was checked above. On success the pointer is writable for
        // `frames` frames of the format set in Initialize until ReleaseBuffer.
        let data = unsafe { self.render.GetBuffer(frames) }
            .map_err(|e| self.endpoint.error(e, "write buffer"))?;
        if !data.is_null() {
            // SAFETY: `data` is writable for `pcm.len() * 4` bytes (one 4-byte sample per
            // item) until ReleaseBuffer, and the device buffer cannot overlap `pcm`.
            let bytes = unsafe { std::slice::from_raw_parts_mut(data, pcm.len() * 4) };
            for (out, sample) in bytes.chunks_exact_mut(4).zip(pcm) {
                out.copy_from_slice(&sample.to_le_bytes());
            }
        }
        // SAFETY: releases exactly the frames GetBuffer gave, now written.
        unsafe { self.render.ReleaseBuffer(frames, 0) }
            .map_err(|e| self.endpoint.error(e, "release buffer"))?;
        if !self.started {
            self.endpoint.start()?;
            self.started = true;
        }
        Ok(())
    }

    /// Audio queued on the device, in milliseconds.
    pub fn queued_ms(&self) -> Result<u32, MediaError> {
        // SAFETY: the client is initialised.
        let queued = unsafe { self.endpoint.client.GetCurrentPadding() }
            .map_err(|e| self.endpoint.error(e, "queue"))?;
        Ok(queued * 1_000 / SAMPLE_RATE)
    }
}
