//! Windows H.264 decoding through Microsoft's Media Foundation decoder, to NV12.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: the [`VideoDecoder`] behind [`MfDecoder`]. It uses Microsoft's H.264 decoder
//! transform, which ships with Windows. Veil bundles no decoder.
//!
//! Invariants:
//! - Low latency mode is on, so one picture comes out for each access unit that goes in. A
//!   picture still held is taken out before the next input (`MF_E_NOTACCEPTING`).
//! - The bitstream comes from another member. Windows parses it; Veil checks the size the
//!   decoder reports ([`check_size`]) before allocating, and copies every picture out of the
//!   locked buffer within the bounds Windows gives ([`copy_nv12`]).
//! - H.264 pads the coded height to whole macroblocks (1080 becomes 1088). The chroma plane
//!   starts after the padded rows; the picture handed back is cropped to the display area.
//! - Field order in [`MfDecoder`] matters: the transform is released before Media Foundation
//!   and COM are shut down.
//!
//! shortcut: decodes on the CPU. The Microsoft decoder uses the GPU (DXVA) only with a D3D11
//! device manager; add one when rendering moves to wgpu textures, so pictures stay on the GPU.

use std::mem::ManuallyDrop;

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaType, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MFCreateMediaType, MFMediaType_Video,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFVideoFormat_H264, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::core::Interface;

use super::sample::{Nv12Layout, copy_nv12, empty_sample, input_sample};
use super::{Runtime, shut_down};
use crate::decoder::{VideoDecoder, check_size};
use crate::{MediaError, RawPicture};

/// Output attempts per access unit: a stream change (new size) costs one each.
const OUTPUT_TRIES: usize = 4;

/// An H.264 decoder for one sender's spatial layer.
pub struct MfDecoder {
    transform: IMFTransform,
    provides_samples: bool,
    output_size: u32,
    /// Where pictures sit in the output buffer, from the current output type.
    layout: Option<Nv12Layout>,
    picture: Vec<u8>,
    _runtime: Runtime,
}

impl MfDecoder {
    /// Opens Microsoft's H.264 decoder. [`MediaError::Unavailable`] means this Windows has
    /// none (some N editions ship without media components).
    pub fn open() -> Result<Self, MediaError> {
        let runtime = Runtime::start()?;
        // SAFETY: the class id is a static GUID and no outer object is passed.
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                .map_err(|_| MediaError::Unavailable("H.264 decoder"))?;
        match configure(&transform) {
            Ok(()) => {}
            Err(e) => {
                shut_down(&transform);
                return Err(e);
            }
        }
        let mut decoder = Self {
            transform,
            provides_samples: false,
            output_size: 1,
            layout: None,
            picture: Vec::new(),
            _runtime: runtime,
        };
        decoder.read_output_type()?;
        Ok(decoder)
    }

    /// Takes at most one picture out of the transform into `self.picture`. `Ok(None)` when it
    /// needs more input.
    fn pull(&mut self) -> Result<Option<u64>, MediaError> {
        for _ in 0..OUTPUT_TRIES {
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
                    let sample = sample.ok_or(MediaError::Decoder("no output sample"))?;
                    let layout = self.layout.ok_or(MediaError::Decoder("no output type"))?;
                    copy_nv12(&sample, layout, &mut self.picture, MediaError::Decoder)?;
                    // SAFETY: `sample` is live. A missing time is not an error: use 0.
                    let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
                    return Ok(Some(u64::try_from(time / 10).unwrap_or(0)));
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.renegotiate()?,
                Err(_) => return Err(MediaError::Decoder("process output")),
            }
        }
        Err(MediaError::Decoder("output format keeps changing"))
    }

    /// Picks NV12 again after a stream change (a keyframe with a new size).
    fn renegotiate(&mut self) -> Result<(), MediaError> {
        set_nv12_output(&self.transform)?;
        self.read_output_type()
    }

    /// Reads the output buffer's layout and the display size from the current output type.
    /// Before the first keyframe the size may be unknown; `layout` is `None` until it is.
    fn read_output_type(&mut self) -> Result<(), MediaError> {
        // SAFETY: `transform` is live; stream 0 is its only output stream.
        let info = unsafe { self.transform.GetOutputStreamInfo(0) }
            .map_err(|_| MediaError::Decoder("output info"))?;
        self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        self.output_size = info.cbSize.max(1);
        // SAFETY: `transform` is live and its output type was set.
        let Ok(current) = (unsafe { self.transform.GetOutputCurrentType(0) }) else {
            self.layout = None;
            return Ok(());
        };
        self.layout = layout(&current)?;
        Ok(())
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(
        &mut self,
        bitstream: &[u8],
        timestamp_us: u64,
    ) -> Result<Option<RawPicture<'_>>, MediaError> {
        if bitstream.is_empty() {
            return Err(MediaError::Malformed("empty access unit"));
        }
        let sample = input_sample(bitstream, timestamp_us, 0)?;
        // SAFETY: `sample` is a live sample holding one access unit.
        let mut result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        if result
            .as_ref()
            .is_err_and(|e| e.code() == MF_E_NOTACCEPTING)
        {
            // A picture is still waiting; it is older than this input, so drop it.
            self.pull()?;
            // SAFETY: as above.
            result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        }
        result.map_err(|_| MediaError::Decoder("process input"))?;
        let Some(time) = self.pull()? else {
            return Ok(None);
        };
        let layout = self.layout.ok_or(MediaError::Decoder("no output type"))?;
        RawPicture::new(
            layout.width,
            layout.height,
            usize::from(layout.width),
            time,
            &self.picture,
        )
        .map(Some)
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // SAFETY: `transform` is live; ending streaming takes no pointers.
        let _ = unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
        };
        shut_down(&self.transform);
    }
}

/// Low latency, H.264 in, NV12 out, then starts streaming.
fn configure(transform: &IMFTransform) -> Result<(), MediaError> {
    // Low latency is what makes one picture come out per access unit. The attribute and the
    // codec setting are the same switch; decoders differ in which they read.
    // SAFETY: `transform` is live; the attribute key is a static GUID.
    if let Ok(attributes) = unsafe { transform.GetAttributes() } {
        // SAFETY: `attributes` is live; the key is a static GUID.
        let _ = unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) };
    }
    if let Ok(codec) = transform.cast::<ICodecAPI>() {
        // SAFETY: the key and value are valid for the call; the decoder copies the value.
        let _ = unsafe { codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)) };
    }
    // SAFETY: no pointers are passed; the result is an owned interface.
    let input = unsafe { MFCreateMediaType() }.map_err(|_| MediaError::Decoder("input type"))?;
    // SAFETY: `input` is live; the keys are static GUIDs. The size comes from the bitstream.
    unsafe {
        input
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|_| MediaError::Decoder("input type"))?;
        input
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|_| MediaError::Decoder("input type"))?;
        transform
            .SetInputType(0, &input, 0)
            .map_err(|_| MediaError::Decoder("input type"))?;
    }
    set_nv12_output(transform)?;
    // SAFETY: `transform` is live; these messages take no pointers.
    unsafe {
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|_| MediaError::Decoder("begin streaming"))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|_| MediaError::Decoder("start stream"))?;
    }
    Ok(())
}

/// Chooses NV12 from the output types the decoder offers.
fn set_nv12_output(transform: &IMFTransform) -> Result<(), MediaError> {
    for index in 0.. {
        // SAFETY: `transform` is live; an index past the last type returns an error.
        let Ok(offered) = (unsafe { transform.GetOutputAvailableType(0, index) }) else {
            break;
        };
        // SAFETY: `offered` is live; the key is a static GUID.
        let subtype = unsafe { offered.GetGUID(&MF_MT_SUBTYPE) };
        if subtype.is_ok_and(|s| s == MFVideoFormat_NV12) {
            // SAFETY: `offered` is a live media type from the transform itself.
            return unsafe { transform.SetOutputType(0, &offered, 0) }
                .map_err(|_| MediaError::Decoder("output type"));
        }
    }
    Err(MediaError::Decoder("no NV12 output"))
}

/// The picture's place in the output buffer, or `None` while the size is not known yet.
fn layout(media: &IMFMediaType) -> Result<Option<Nv12Layout>, MediaError> {
    // SAFETY: `media` is live; the key is a static GUID.
    let Ok(size) = (unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE) }) else {
        return Ok(None);
    };
    let (coded_w, coded_h) = ((size >> 32) as u32, (size & 0xFFFF_FFFF) as u32);
    if coded_w == 0 || coded_h == 0 {
        return Ok(None);
    }
    let (coded_w, coded_h) = check_size(coded_w, coded_h)?;
    let (width, height) = match display_area(media) {
        Some((w, h)) if w <= u32::from(coded_w) && h <= u32::from(coded_h) => check_size(w, h)?,
        _ => (coded_w, coded_h),
    };
    // SAFETY: `media` is live; the key is a static GUID. A missing stride means packed rows.
    let stride = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .ok()
        .and_then(|s| usize::try_from(s as i32).ok())
        .filter(|&s| s >= usize::from(coded_w))
        .unwrap_or(usize::from(coded_w));
    Ok(Some(Nv12Layout {
        width,
        height,
        stride,
        alloc_rows: usize::from(coded_h),
    }))
}

/// The display area's size when it starts at the top left corner, as H.264 cropping does.
fn display_area(media: &IMFMediaType) -> Option<(u32, u32)> {
    // MFVideoArea: two 4-byte offsets (fraction, value), then the size as two i32.
    let mut blob = [0u8; 16];
    let mut len = 0u32;
    // SAFETY: `blob` is a writable buffer whose length is passed with it, and `len` is a valid
    // out value. The call fails rather than write past `blob`.
    unsafe { media.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, Some(&mut len)) }.ok()?;
    if len != 16 || blob[..8].iter().any(|&b| b != 0) {
        return None;
    }
    let cx = i32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
    let cy = i32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
    Some((u32::try_from(cx).ok()?, u32::try_from(cy).ok()?))
}
