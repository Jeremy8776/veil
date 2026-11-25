//! Windows Media Foundation: H.264 encoding, one encoder transform per simulcast rung, H.264
//! decoding (`decoder.rs`), camera capture (`camera.rs`), and microphone and speakers through
//! WASAPI (`audio.rs`), and computer sound for a screen share (`loopback.rs`).
//!
//! Purpose: the [`RungEncoder`] behind [`MediaFoundationEncoder`]. It prefers a hardware
//! encoder (GPU, through `MFTEnumEx`) and falls back to Microsoft's software H.264 encoder,
//! which ships with Windows. Veil bundles neither.
//!
//! Invariants:
//! - This is the only `unsafe` code on Windows. Every block says why it is sound.
//! - Every COM pointer is owned by a `windows` interface type, so it is released exactly once.
//!   The two places that receive raw ownership (the `MFTEnumEx` array and the
//!   `MFT_OUTPUT_DATA_BUFFER` fields) move each pointer out exactly once.
//! - Low latency, no B pictures, constant bitrate, no encoder-chosen IDR: one access unit comes
//!   out for every picture that goes in, before `encode` returns. An encoder that holds
//!   pictures back is refused rather than waited on.
//! - A rung with temporal layers sets `CODECAPI_AVEncVideoTemporalLayerCount`, and an encoder
//!   that cannot is skipped. [`crate::simulcast`] checks the result.
//! - Field order in [`MfRung`] matters: the transform is released before Media Foundation and
//!   COM are shut down.

#![allow(unsafe_code)]

mod app_window;
mod attention;
mod audio;
mod audio_devices;
mod camera;
mod decoder;
mod enumerate;
mod loopback;
mod protect;
mod sample;
mod screen_stream;

pub use app_window::{is_app_window, set_hidden_from_capture};
pub use attention::{clear_notifications, show_notification};
pub use audio::{WasapiCapture, WasapiPlayback};
pub use audio_devices::{AudioDevice, microphones, speakers};
pub use camera::{CameraInfo, MfCamera, cameras};
pub use decoder::MfDecoder;
pub use loopback::{LoopbackCapture, loopback_support};
pub use protect::{protect, unprotect};
pub use screen_stream::ScreenStream;

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoTemporalLayerCount,
    CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaEventGenerator, IMFMediaType, IMFShutdown,
    IMFTransform, METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
    MFCreateMediaType, MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Main,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};

use crate::MediaError;
use crate::ladder::{Ladder, Rung};
use crate::simulcast::{RungEncoder, Simulcast};
use enumerate::{hardware_transforms, software_transform};
use sample::{empty_sample, input_sample, read_sample};

/// Simulcast H.264 through Media Foundation, one encoder per rung.
pub type MediaFoundationEncoder = Simulcast<MfRung>;

/// Opens one encoder per rung of `ladder`. With `hardware`, a GPU encoder is tried first for
/// each rung; without, only Microsoft's software encoder is used.
/// [`MediaError::Unavailable`] means this machine has no usable encoder for some rung.
pub fn media_foundation(
    ladder: &Ladder,
    hardware: bool,
) -> Result<MediaFoundationEncoder, MediaError> {
    Simulcast::new(ladder, |rung, fps| MfRung::open(rung, fps, hardware))
}

/// The longest an encoder may take to return a picture before it is treated as stuck.
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(2);
/// Large enough that the encoder never inserts an IDR of its own between forced keyframes.
const GOP_NEVER: u32 = 0xFFFF_FFFE;

/// Keeps COM and Media Foundation started while an encoder, decoder or camera lives. Each
/// owner holds it as its last field, so its interfaces are released first.
struct Runtime {
    uninit_com: bool,
}

impl Runtime {
    fn start() -> Result<Self, MediaError> {
        // SAFETY: no pointers are passed. Any success (S_OK or S_FALSE) must be balanced by
        // CoUninitialize on this thread; a failure means the thread is already in another
        // apartment, which Media Foundation accepts, and must not be balanced.
        let uninit_com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        // SAFETY: no pointers are passed. Balanced by MFShutdown in Drop.
        if unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.is_err() {
            if uninit_com {
                // SAFETY: balances the successful CoInitializeEx above, on the same thread.
                unsafe { CoUninitialize() };
            }
            return Err(MediaError::Unavailable("Media Foundation"));
        }
        Ok(Self { uninit_com })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: balances the MFStartup in `start`. Every interface of the owner has been
        // released already (the runtime is its last field).
        let _ = unsafe { MFShutdown() };
        if self.uninit_com {
            // SAFETY: balances the successful CoInitializeEx in `start`. Every owner holds COM
            // interfaces, so it is not Send, and this runs on the thread that called it.
            unsafe { CoUninitialize() };
        }
    }
}

/// One rung's Media Foundation encoder.
pub struct MfRung {
    transform: IMFTransform,
    codec: ICodecAPI,
    /// Present for an asynchronous (hardware) transform, which signals with events.
    events: Option<IMFMediaEventGenerator>,
    provides_samples: bool,
    output_size: u32,
    input_len: usize,
    frame_100ns: i64,
    need_input: u32,
    name: String,
    hardware: bool,
    /// What `open` was given, to open again on restart.
    rung: Rung,
    fps: u32,
    prefer_hardware: bool,
    _runtime: Runtime,
}

impl MfRung {
    fn open(rung: &Rung, fps: u32, hardware: bool) -> Result<Self, MediaError> {
        let runtime = Runtime::start()?;
        let mut candidates = if hardware {
            hardware_transforms()
        } else {
            Vec::new()
        };
        if let Some(software) = software_transform() {
            candidates.push((
                software,
                String::from("Microsoft H.264 software encoder"),
                false,
            ));
        }
        for (transform, name, is_hardware) in candidates {
            if let Ok(parts) = configure(&transform, rung, fps, is_hardware) {
                let frame_100ns = 10_000_000 / i64::from(fps.max(1));
                return Ok(Self {
                    transform,
                    codec: parts.codec,
                    events: parts.events,
                    provides_samples: parts.provides_samples,
                    output_size: parts.output_size,
                    input_len: usize::from(rung.width) * usize::from(rung.height) * 3 / 2,
                    frame_100ns,
                    need_input: 0,
                    name,
                    hardware: is_hardware,
                    rung: *rung,
                    fps,
                    prefer_hardware: hardware,
                    _runtime: runtime,
                });
            }
            shut_down(&transform);
        }
        Err(MediaError::Unavailable("H.264 encoder"))
    }

    /// The encoder's friendly name, for logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// True for a GPU encoder, false for the software fallback.
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Waits for the next transform event: need input or have output.
    fn next_event(
        &mut self,
        events: &IMFMediaEventGenerator,
        deadline: Instant,
    ) -> Result<u32, MediaError> {
        loop {
            // SAFETY: `events` is a live interface; NO_WAIT returns at once with an event or
            // MF_E_NO_EVENTS_AVAILABLE.
            match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    // SAFETY: `event` is a live interface returned to us above.
                    return unsafe { event.GetType() }
                        .map_err(|_| MediaError::Encoder("event type"));
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Err(MediaError::Encoder("encoder holds pictures"));
                    }
                    // shortcut: polls every 0.2 ms. Use BeginGetEvent with a callback if the
                    // wait shows up in profiles.
                    std::thread::sleep(Duration::from_micros(200));
                }
                Err(_) => return Err(MediaError::Encoder("encoder event")),
            }
        }
    }

    /// Takes one access unit from the transform.
    fn pull_output(&mut self) -> Result<Vec<u8>, MediaError> {
        for _ in 0..3 {
            let provided = if self.provides_samples {
                None
            } else {
                Some(empty_sample(self.output_size)?)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: `buffers` is a valid one-element array and `status` a valid out value,
            // both alive for the call. The transform writes owned references into the fields.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: each field is taken exactly once, so the references the transform left
            // there (or the sample we put in) are released exactly once.
            let (sample, _events) = unsafe {
                (
                    ManuallyDrop::take(&mut buffers[0].pSample),
                    ManuallyDrop::take(&mut buffers[0].pEvents),
                )
            };
            match result {
                Ok(()) => {
                    return sample
                        .ok_or(MediaError::Encoder("no output sample"))
                        .and_then(|s| read_sample(&s));
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.renegotiate()?,
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Err(MediaError::Encoder("encoder holds pictures"));
                }
                Err(_) => return Err(MediaError::Encoder("process output")),
            }
        }
        Err(MediaError::Encoder("output format keeps changing"))
    }

    /// Accepts the output type the transform offers after a stream change.
    fn renegotiate(&mut self) -> Result<(), MediaError> {
        // SAFETY: `transform` is live; stream 0, type 0 exists after a stream change.
        let offered =
            unsafe { self.transform.GetOutputAvailableType(0, 0) }.map_err(step("output type"))?;
        // SAFETY: `offered` is a live media type from the transform itself.
        unsafe { self.transform.SetOutputType(0, &offered, 0) }.map_err(step("output type"))?;
        let (provides, size) = output_info(&self.transform)?;
        self.provides_samples = provides;
        self.output_size = size;
        Ok(())
    }
}

impl RungEncoder for MfRung {
    fn encode(
        &mut self,
        nv12: &[u8],
        timestamp_us: u64,
        keyframe: bool,
    ) -> Result<Vec<u8>, MediaError> {
        if nv12.len() != self.input_len {
            return Err(MediaError::Malformed("rung picture size"));
        }
        let sample = input_sample(nv12, timestamp_us, self.frame_100ns)?;
        if keyframe {
            set(
                &self.codec,
                &CODECAPI_AVEncVideoForceKeyFrame,
                VARIANT::from(1u32),
            )?;
        }
        let deadline = Instant::now() + OUTPUT_TIMEOUT;
        let events = self.events.clone();
        if let Some(events) = &events {
            while self.need_input == 0 {
                match self.next_event(events, deadline)? {
                    t if t == METransformNeedInput.0 as u32 => self.need_input += 1,
                    t if t == METransformHaveOutput.0 as u32 => {
                        return Err(MediaError::Encoder("output without input"));
                    }
                    _ => {}
                }
            }
        }
        // SAFETY: `sample` is a live sample holding a buffer of exactly one picture.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(step("process input"))?;
        let Some(events) = events else {
            return self.pull_output();
        };
        self.need_input -= 1;
        loop {
            match self.next_event(&events, deadline)? {
                t if t == METransformNeedInput.0 as u32 => self.need_input += 1,
                t if t == METransformHaveOutput.0 as u32 => return self.pull_output(),
                _ => {}
            }
        }
    }

    fn restart(&mut self) -> Result<(), MediaError> {
        self.reopen()
    }
}

impl MfRung {
    /// Replaces this encoder with a fresh one. The old one is released after the new one opens.
    ///
    /// shortcut: reopening costs tens of milliseconds on the encoding thread. Media Foundation
    /// has no portable way to restart the temporal pattern in place; revisit per vendor if the
    /// stall on resume is noticeable.
    fn reopen(&mut self) -> Result<(), MediaError> {
        *self = Self::open(&self.rung, self.fps, self.prefer_hardware)?;
        Ok(())
    }
}

impl Drop for MfRung {
    fn drop(&mut self) {
        // SAFETY: `transform` is live; ending streaming takes no pointers.
        let _ = unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
        };
        shut_down(&self.transform);
    }
}

/// What `configure` learns about a transform it set up.
struct Parts {
    codec: ICodecAPI,
    events: Option<IMFMediaEventGenerator>,
    provides_samples: bool,
    output_size: u32,
}

/// Sets the rung's types and encoder settings, then starts streaming.
fn configure(
    transform: &IMFTransform,
    rung: &Rung,
    fps: u32,
    hardware: bool,
) -> Result<Parts, MediaError> {
    let codec: ICodecAPI = transform.cast().map_err(step("codec API"))?;
    let events = if hardware {
        unlock_async(transform)?
    } else {
        None
    };

    // Optional: an encoder that ignores one of these still encodes, just less suited to calls.
    let _ = set(&codec, &CODECAPI_AVLowLatencyMode, VARIANT::from(true));
    let _ = set(
        &codec,
        &CODECAPI_AVEncCommonRateControlMode,
        VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
    );
    let _ = set(
        &codec,
        &CODECAPI_AVEncCommonMeanBitRate,
        VARIANT::from(rung.bitrate_bps),
    );
    let _ = set(
        &codec,
        &CODECAPI_AVEncMPVDefaultBPictureCount,
        VARIANT::from(0u32),
    );
    let _ = set(&codec, &CODECAPI_AVEncMPVGOPSize, VARIANT::from(GOP_NEVER));

    let levels = u32::from(rung.top_temporal) + 1;
    if levels > 1 {
        set(
            &codec,
            &CODECAPI_AVEncVideoTemporalLayerCount,
            VARIANT::from(levels),
        )?;
    }

    let size = (u64::from(rung.width) << 32) | u64::from(rung.height);
    let rate = (u64::from(fps) << 32) | 1;
    let output = media_type(MFVideoFormat_H264, size, rate)?;
    // SAFETY: `output` is a live media type; the attribute keys are static GUIDs.
    unsafe {
        output
            .SetUINT32(&MF_MT_AVG_BITRATE, rung.bitrate_bps)
            .map_err(step("output type"))?;
        output
            .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
            .map_err(step("output type"))?;
        transform
            .SetOutputType(0, &output, 0)
            .map_err(step("output type"))?;
    }
    let input = media_type(MFVideoFormat_NV12, size, rate)?;
    // SAFETY: `input` is a live media type.
    unsafe { transform.SetInputType(0, &input, 0) }.map_err(step("input type"))?;

    let (provides_samples, output_size) = output_info(transform)?;
    // SAFETY: `transform` is live; these messages take no pointers.
    unsafe {
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(step("begin streaming"))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(step("start stream"))?;
    }
    Ok(Parts {
        codec,
        events,
        provides_samples,
        output_size,
    })
}

/// An asynchronous transform must be unlocked before use, and signals through events.
fn unlock_async(transform: &IMFTransform) -> Result<Option<IMFMediaEventGenerator>, MediaError> {
    // SAFETY: `transform` is live; the attribute keys are static GUIDs.
    let is_async = unsafe {
        let attributes = transform.GetAttributes().map_err(step("attributes"))?;
        let is_async = attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
        if is_async {
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(step("unlock"))?;
        }
        is_async
    };
    if !is_async {
        return Ok(None);
    }
    transform.cast().map(Some).map_err(step("event generator"))
}

fn media_type(subtype: GUID, size: u64, rate: u64) -> Result<IMFMediaType, MediaError> {
    // SAFETY: no pointers are passed; the result is an owned interface.
    let media = unsafe { MFCreateMediaType() }.map_err(step("media type"))?;
    // SAFETY: `media` is live; the attribute keys are static GUIDs.
    unsafe {
        media
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(step("media type"))?;
        media
            .SetGUID(&MF_MT_SUBTYPE, &subtype)
            .map_err(step("media type"))?;
        media
            .SetUINT64(&MF_MT_FRAME_SIZE, size)
            .map_err(step("media type"))?;
        media
            .SetUINT64(&MF_MT_FRAME_RATE, rate)
            .map_err(step("media type"))?;
        media
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(step("media type"))?;
    }
    Ok(media)
}

/// Whether the transform allocates output samples, and how large ours must be if not.
fn output_info(transform: &IMFTransform) -> Result<(bool, u32), MediaError> {
    // SAFETY: `transform` is live; stream 0 is its only output stream.
    let info = unsafe { transform.GetOutputStreamInfo(0) }.map_err(step("output info"))?;
    let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    Ok((provides, info.cbSize.max(1)))
}

fn set(codec: &ICodecAPI, key: &GUID, value: VARIANT) -> Result<(), MediaError> {
    // SAFETY: `key` and `value` are valid for the call; the encoder copies the value.
    unsafe { codec.SetValue(key, &value) }.map_err(step("encoder setting"))
}

/// Releases an asynchronous transform's worker threads; harmless for one that has none.
fn shut_down(transform: &IMFTransform) {
    if let Ok(shutdown) = transform.cast::<IMFShutdown>() {
        // SAFETY: `shutdown` is a live interface; Shutdown takes no pointers.
        let _ = unsafe { shutdown.Shutdown() };
    }
}

fn step(name: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |_| MediaError::Encoder(name)
}
