//! macOS computer sound through ScreenCaptureKit, without Veil's own playback. Part of the
//! macOS `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: [`SckAudio`] runs a ScreenCaptureKit stream of the main display that captures
//! audio only (its pictures are tiny, once a second, and never read) with
//! `excludesCurrentProcessAudio` on, so the call's other voices are not sent back to them.
//! Sound arrives on a dispatch queue and goes to the reader through a `Feed`.
//!
//! Invariants:
//! - Needs macOS 13 or later and the Screen Recording permission a screen share already asks
//!   for. [`sck_audio_support`] checks the version; a refused permission fails the open.
//! - ScreenCaptureKit hands over 48 kHz float stereo, planar or interleaved; both are read.
//! - Each block buffer that holds sound is released after it is copied, on every path.
//! - Dropping the capture stops the stream.

use std::ptr::NonNull;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSOperatingSystemVersion, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use crate::MediaError;
use crate::system_sound::{Assembler, Feed, Support, push_stereo};

/// The longest macOS may take to list what can be captured.
const CONTENT_WAIT: Duration = Duration::from_secs(3);
/// `kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment`.
const ALIGN_16: u32 = 1;

/// Whether this macOS can leave Veil's own sound out of a capture (macOS 13 or later).
pub fn sck_audio_support() -> Support {
    static FOUND: OnceLock<Support> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let version = NSOperatingSystemVersion {
                majorVersion: 13,
                minorVersion: 0,
                patchVersion: 0,
            };
            if NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(version) {
                Support::Ready
            } else {
                Support::Unavailable("Needs macOS 13 or later")
            }
        })
        .clone()
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds no Drop impl.
    #[unsafe(super(NSObject))]
    #[name = "VeilShareSoundOutput"]
    #[ivars = Arc<Feed>]
    struct Output;

    // SAFETY: NSObject already conforms; nothing is added.
    unsafe impl NSObjectProtocol for Output {}

    // SAFETY: the method signature matches the protocol's.
    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Audio {
                copy_sound(sample, self.ivars());
            }
        }
    }
);

impl Output {
    fn new(feed: Arc<Feed>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(feed);
        // SAFETY: NSObject's designated initialiser, called once on a fresh allocation.
        unsafe { msg_send![super(this), init] }
    }
}

/// Copies the sound in `sample` to `feed` as interleaved stereo.
fn copy_sound(sample: &CMSampleBuffer, feed: &Feed) {
    let mut needed = 0usize;
    // SAFETY: asks only for the size of the buffer list; every other pointer is null.
    unsafe {
        sample.audio_buffer_list_with_retained_block_buffer(
            &mut needed,
            std::ptr::null_mut(),
            0,
            None,
            None,
            ALIGN_16,
            std::ptr::null_mut(),
        )
    };
    if needed < size_of::<AudioBufferList>() {
        return;
    }
    // u64 words keep the list aligned for its pointers.
    let mut storage = vec![0u64; needed.div_ceil(8)];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: `list` points to `needed` writable, aligned bytes, and `block` is a valid out
    // pointer. On success the list's buffers point into `block`, which we then own.
    let status = unsafe {
        sample.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            list,
            needed,
            None,
            None,
            ALIGN_16,
            &mut block,
        )
    };
    // SAFETY: a non-null `block` is a +1 reference the call gave us; released when dropped.
    let _block = NonNull::new(block).map(|b| unsafe { CFRetained::from_raw(b) });
    if status != 0 {
        return;
    }
    // SAFETY: the call filled the list header; `mNumberBuffers` buffers follow it within
    // `needed` bytes.
    let count = unsafe { (*list).mNumberBuffers } as usize;
    // SAFETY: `mBuffers` is the first of `count` buffers laid out in a row in `storage`.
    let buffers: &[AudioBuffer] =
        unsafe { std::slice::from_raw_parts((&raw const (*list).mBuffers).cast(), count) };
    let samples = |buffer: &AudioBuffer| -> &[f32] {
        if buffer.mData.is_null() {
            return &[];
        }
        let len = buffer.mDataByteSize as usize / size_of::<f32>();
        // SAFETY: the buffer holds `mDataByteSize` bytes of 32-bit float samples, kept alive by
        // `_block` until the copy below has finished.
        unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>().cast_const(), len) }
    };
    let mut stereo = Vec::new();
    match buffers {
        [one] => push_stereo(samples(one), one.mNumberChannels as usize, &mut stereo),
        [left, right, ..] => {
            let (left, right) = (samples(left), samples(right));
            stereo.extend(left.iter().zip(right).flat_map(|(l, r)| [*l, *r]));
        }
        [] => return,
    }
    feed.push(&stereo);
}

/// A value that may move to another thread.
struct Sendable<T>(T);

// SAFETY: only immutable ScreenCaptureKit content lists cross threads here, which Apple
// documents as safe to use from any thread.
unsafe impl<T> Send for Sendable<T> {}

/// What macOS can capture, waited for.
fn shareable_content() -> Result<Retained<SCShareableContent>, MediaError> {
    let (sender, answer): (Sender<Sendable<Option<Retained<SCShareableContent>>>>, _) = channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, _error: *mut NSError| {
            // SAFETY: `content` is null or a live object for the duration of the block; retaining
            // it keeps it past the block.
            let content = unsafe { Retained::retain(content) };
            let _ = sender.send(Sendable(content));
        },
    );
    // SAFETY: the block has the signature the method documents, and outlives the call.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    answer
        .recv_timeout(CONTENT_WAIT)
        .ok()
        .and_then(|content| content.0)
        .ok_or(MediaError::Capture("Screen Recording permission"))
}

/// Everything macOS plays, except Veil's own sound.
pub struct SckAudio {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    feed: Arc<Feed>,
    frames: Assembler,
}

impl SckAudio {
    pub fn open() -> Result<Self, MediaError> {
        if sck_audio_support() != Support::Ready {
            return Err(MediaError::Unavailable("computer sound"));
        }
        let settings = crate::audio::AudioSettings::share_sound();
        let content = shareable_content()?;
        // SAFETY: a getter on a live object.
        let displays = unsafe { content.displays() };
        let display = displays
            .firstObject()
            .ok_or(MediaError::Unavailable("display"))?;
        // SAFETY: a fresh allocation, a live display and an empty list of windows to leave out.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            )
        };
        // SAFETY: a fresh configuration, set before the stream is made from it.
        let config = unsafe { SCStreamConfiguration::new() };
        // SAFETY: plain setters on the configuration, with values in their documented ranges.
        unsafe {
            config.setCapturesAudio(true);
            config.setExcludesCurrentProcessAudio(true);
            config.setSampleRate(48_000);
            config.setChannelCount(2);
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: 1,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            });
        }
        // SAFETY: a fresh allocation with a live filter and configuration, and no delegate.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };
        let feed = Arc::new(Feed::default());
        let output = Output::new(feed.clone());
        let queue = DispatchQueue::new("veil.share-sound", None);
        // SAFETY: an output that conforms to the protocol and a serial queue, as required.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Audio,
                Some(&queue),
            )
        }
        .map_err(|_| MediaError::Capture("computer sound output"))?;
        let started = feed.clone();
        let done = RcBlock::new(move |error: *mut NSError| {
            if !error.is_null() {
                started.end("computer sound refused");
            }
        });
        // SAFETY: the stream is configured, and the block has the documented signature.
        unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) };
        Ok(Self {
            stream,
            _output: output,
            _queue: queue,
            feed,
            frames: Assembler::new(&settings),
        })
    }

    /// Waits for the next frame of [`crate::AudioSettings::share_sound`], at most until it
    /// is due.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        self.feed.read(&mut self.frames)
    }
}

impl Drop for SckAudio {
    fn drop(&mut self) {
        // SAFETY: stops a live stream; no completion is needed.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}
