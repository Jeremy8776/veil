//! Windows computer sound through WASAPI process loopback, without Veil's own playback.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: [`LoopbackCapture`] reads everything Windows plays except what Veil's own process
//! tree plays, so the call's other voices are not sent back to them. Windows mixes and converts
//! the sound itself; Veil asks for 48 kHz float stereo ([`AudioSettings::share_sound`]).
//!
//! Invariants:
//! - Activation is asynchronous: the completion handler only passes the result over a channel,
//!   and the opening thread waits at most [`ACTIVATE_WAIT`].
//! - Process loopback needs Windows 10 version 2004 or later. [`loopback_support`] opens and
//!   closes one capture, once per run, to find out.
//! - A process loopback client sends no events while nothing plays, so reading waits on the
//!   event only until the frame is due (`system_sound::Assembler`) and then hands over silence.
//! - The activation parameters outlive the call that reads them; the blob points into a local
//!   that lives until activation has finished, and is never cleared (it owns nothing).

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{BLOB, IAgileObject};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcessId, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, Ref, implement};

use super::Runtime;
use super::audio::float_format;
use crate::MediaError;
use crate::audio::AudioSettings;
use crate::system_sound::{Assembler, Support};

/// The longest Windows may take to activate the loopback client.
const ACTIVATE_WAIT: Duration = Duration::from_secs(3);

/// An auto-reset event the audio engine signals, closed on drop.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed exactly once, after the
        // client that signalled it has been released (field order in `LoopbackCapture`).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Passes the activation result to the thread that asked.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated(Sender<Result<IAudioClient, MediaError>>);

/// A marker: the handler may be called on any thread, which activation requires.
impl windows::Win32::System::Com::IAgileObject_Impl for Activated_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = operation
            .ok()
            .ok()
            .and_then(|operation| {
                let (mut code, mut client) = (HRESULT(0), None::<IUnknown>);
                // SAFETY: both are valid out values for the duration of the call.
                unsafe { operation.GetActivateResult(&mut code, &mut client) }.ok()?;
                code.ok().ok()?;
                client?.cast::<IAudioClient>().ok()
            })
            .ok_or(MediaError::Unavailable("computer sound"));
        let _ = self.0.send(result);
        Ok(())
    }
}

/// Whether process loopback works on this Windows. Found once per run.
pub fn loopback_support() -> Support {
    static FOUND: OnceLock<Support> = OnceLock::new();
    FOUND
        .get_or_init(|| match LoopbackCapture::open() {
            Ok(_) => Support::Ready,
            Err(_) => Support::Unavailable("Needs Windows 10 version 2004 or later"),
        })
        .clone()
}

/// Everything Windows plays, except Veil's own sound.
pub struct LoopbackCapture {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: Event,
    frames: Assembler,
    _runtime: Runtime,
}

impl LoopbackCapture {
    /// Opens and starts the capture. [`MediaError::Unavailable`] means this Windows cannot.
    pub fn open() -> Result<Self, MediaError> {
        let settings = AudioSettings::share_sound();
        let runtime = Runtime::start()?;
        let client = activate()?;
        let format = float_format(settings.channels);
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // Five frames of buffer, in 100 ns units.
        let duration = i64::from(settings.frame_ms) * 5 * 10_000;
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
        .map_err(|_| MediaError::Capture("computer sound format"))?;
        // SAFETY: no pointers are passed: an unnamed auto-reset event, not signalled.
        let event = Event(
            unsafe { CreateEventW(None, false, false, None) }
                .map_err(|_| MediaError::Capture("event"))?,
        );
        // SAFETY: `client` is live and `event` stays open longer than it (field order).
        unsafe { client.SetEventHandle(event.0) }.map_err(|_| MediaError::Capture("event"))?;
        // SAFETY: the client is initialised for capture.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.map_err(|_| MediaError::Capture("service"))?;
        // SAFETY: `client` is initialised; Start takes no pointers.
        unsafe { client.Start() }.map_err(|_| MediaError::Capture("start"))?;
        Ok(Self {
            capture,
            client,
            event,
            frames: Assembler::new(&settings),
            _runtime: runtime,
        })
    }

    /// Waits for the next frame of [`AudioSettings::share_sound`], at most until it is due.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        loop {
            self.drain()?;
            let now = Instant::now();
            let deadline = self.frames.deadline(now);
            if self.frames.ready() || now >= deadline {
                break;
            }
            let wait = u32::try_from((deadline - now).as_millis())
                .unwrap_or(u32::MAX)
                .max(1);
            // SAFETY: the event handle is open for as long as `self`.
            let _ = unsafe { WaitForSingleObject(self.event.0, wait) } == WAIT_OBJECT_0;
        }
        Ok(self.frames.take())
    }

    /// Moves every packet Windows holds into the assembler.
    fn drain(&mut self) -> Result<(), MediaError> {
        loop {
            // SAFETY: the capture client is live.
            let next = unsafe { self.capture.GetNextPacketSize() }
                .map_err(|_| MediaError::Capture("computer sound lost"))?;
            if next == 0 {
                return Ok(());
            }
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
            // SAFETY: the three are valid out values. On success `data` points to `frames`
            // frames of the format set in Initialize until ReleaseBuffer.
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
            }
            .map_err(|_| MediaError::Capture("read buffer"))?;
            let samples = frames as usize * 2;
            if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                self.frames.push(&vec![0.0; samples]);
            } else {
                // SAFETY: `data` points to `frames` frames of two 4-byte samples while the
                // buffer is held, and the bytes are copied before ReleaseBuffer.
                let bytes = unsafe { std::slice::from_raw_parts(data, samples * 4) };
                let sound: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                self.frames.push(&sound);
            }
            // SAFETY: releases exactly the frames GetBuffer gave.
            unsafe { self.capture.ReleaseBuffer(frames) }
                .map_err(|_| MediaError::Capture("release buffer"))?;
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        // SAFETY: `client` is live; Stop takes no pointers and is harmless when stopped.
        let _ = unsafe { self.client.Stop() };
    }
}

/// Asks Windows for a loopback client of every process except this one and its children.
fn activate() -> Result<IAudioClient, MediaError> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                // SAFETY: takes no arguments and cannot fail.
                TargetProcessId: unsafe { GetCurrentProcessId() },
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // Never cleared: `windows` clears a PROPVARIANT on drop, which would free `params`, a
    // local. Nothing in it is owned.
    let blob = std::mem::ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&raw mut params).cast(),
                    },
                },
                ..Default::default()
            }),
        },
    });
    let (sender, answer) = channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(sender).into();
    // SAFETY: the device path is a static wide string, the IID is IAudioClient's, and `blob`
    // points at `params`; both live until the answer below has arrived or timed out. Windows
    // copies the parameters during the call.
    let _operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *blob),
            &handler,
        )
    }
    .map_err(|_| MediaError::Unavailable("computer sound"))?;
    answer
        .recv_timeout(ACTIVATE_WAIT)
        .map_err(|_| MediaError::Unavailable("computer sound"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens real process loopback on this computer and reads half a second. Ignored by
    /// default: it needs Windows 10 version 2004 or later and a sound device.
    #[test]
    #[ignore = "needs a real Windows sound device"]
    fn loopback_opens_and_reads_frames_at_real_time() -> Result<(), MediaError> {
        let mut capture = LoopbackCapture::open()?;
        let start = Instant::now();
        for _ in 0..25 {
            assert_eq!(capture.read()?.len(), 1_920);
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(400), "{took:?}");
        assert!(took < Duration::from_millis(900), "{took:?}");
        assert_eq!(loopback_support(), Support::Ready);
        Ok(())
    }
}
