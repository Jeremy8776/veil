//! OpenH264, built from source by `openh264-sys2`: H.264 encoding and decoding on Linux and
//! macOS.
//!
//! Purpose: the [`RungEncoder`] behind [`OpenH264Encoder`] (one encoder per simulcast rung)
//! and the [`VideoDecoder`] [`OpenH264Decoder`]. Windows uses Media Foundation instead. The
//! bitstream is plain H.264 (Constrained Baseline), so either side decodes the other.
//!
//! Invariants:
//! - Each encoder and decoder pointer comes from `WelsCreate*`, is never null once stored, is
//!   initialised before it is stored, and is uninitialised and destroyed exactly once, in
//!   `Drop`. Every vtable call goes through a function the vtable has (a missing entry is an
//!   error, never a call through null).
//! - Every picture handed to OpenH264 is a Rust buffer holding the whole I420 picture at the
//!   pitches passed with it. Every buffer OpenH264 hands back is read only within the lengths
//!   it reports, and copied out before the next call into the same object.
//! - Low latency: no frame skipping, constant bitrate, single slice, one thread, no
//!   encoder-chosen IDR. One access unit comes out for every picture that goes in; a skipped
//!   picture is an error, as on Windows.
//! - Temporal layers: `top_temporal + 1` dyadic levels, as [`crate::simulcast`] expects, with
//!   prefix units so it can check each picture's temporal id.
//!
//! shortcut: compiled from Cisco's source, so Cisco's patent licence (which covers only its
//! own downloaded binary) does not apply. See docs/TRANSPARENCY.md. Use VideoToolbox on macOS
//! and VA-API on Linux, or load Cisco's binary at install time, before a wide release.

#![allow(unsafe_code)]

use std::ffi::{c_int, c_void};
use std::ptr::{null_mut, NonNull};

use openh264_sys2::source::APILoader;
use openh264_sys2::{
    CAMERA_VIDEO_REAL_TIME, CONSTANT_ID, DECODER_OPTION_TRACE_LEVEL, ENCODER_OPTION_DATAFORMAT,
    ENCODER_OPTION_TRACE_LEVEL, ERROR_CON_DISABLE, ISVCDecoder, ISVCDecoderVtbl, ISVCEncoder,
    ISVCEncoderVtbl, LOW_COMPLEXITY, PRO_BASELINE, RC_BITRATE_MODE, SBufferInfo, SDecodingParam,
    SEncParamExt, SFrameBSInfo, SM_SINGLE_SLICE, SSourcePicture, VIDEO_BITSTREAM_DEFAULT,
    WELS_LOG_QUIET, dsErrorFree, videoFormatI420, videoFrameTypeInvalid, videoFrameTypeSkip,
};

use crate::MediaError;
use crate::decoder::{VideoDecoder, check_size};
use crate::encoder::RawPicture;
use crate::ladder::{Ladder, Rung};
use crate::simulcast::{RungEncoder, Simulcast};
use crate::yuv::{Plane, i420_to_nv12, nv12_to_i420};

/// Simulcast H.264 through OpenH264, one encoder per rung.
pub type OpenH264Encoder = Simulcast<OpenH264Rung>;

/// Opens one OpenH264 encoder per rung of `ladder`.
pub fn openh264(ladder: &Ladder) -> Result<OpenH264Encoder, MediaError> {
    Simulcast::new(ladder, OpenH264Rung::open)
}

/// OpenH264's own limit on temporal levels.
const MAX_TEMPORAL_LEVELS: u8 = 4;
/// `cmResultSuccess`.
const OK: c_int = 0;

/// One rung's OpenH264 encoder.
pub struct OpenH264Rung {
    encoder: NonNull<ISVCEncoder>,
    rung: Rung,
    fps: u32,
    i420: Vec<u8>,
    /// Large (128 layer records), so boxed.
    info: Box<SFrameBSInfo>,
}

impl OpenH264Rung {
    fn open(rung: &Rung, fps: u32) -> Result<Self, MediaError> {
        let levels = rung.top_temporal.saturating_add(1);
        if levels > MAX_TEMPORAL_LEVELS {
            return Err(MediaError::BadConfig("too many temporal layers for OpenH264"));
        }
        let mut raw: *mut ISVCEncoder = null_mut();
        // SAFETY: `raw` is a valid out pointer for the call.
        let rc = unsafe { APILoader::WelsCreateSVCEncoder(&mut raw) };
        let encoder = NonNull::new(raw).filter(|_| rc == OK);
        let encoder = encoder.ok_or(MediaError::Unavailable("OpenH264 encoder"))?;
        // From here on `Drop` cleans up, including after a failed initialise: OpenH264's
        // Uninitialize is a no-op on an encoder that was never initialised.
        let this = Self {
            encoder,
            rung: *rung,
            fps,
            i420: Vec::new(),
            info: Box::default(),
        };
        let mut params = SEncParamExt::default();
        let vt = this.vtable()?;
        let get_defaults = vt.GetDefaultParams.ok_or(MediaError::Encoder("vtable"))?;
        // SAFETY: the encoder is live and `params` is a valid struct for it to fill.
        if unsafe { get_defaults(this.encoder.as_ptr(), &mut params) } != OK {
            return Err(MediaError::Encoder("default parameters"));
        }
        configure(&mut params, rung, fps, levels)?;
        let initialize = vt.InitializeExt.ok_or(MediaError::Encoder("vtable"))?;
        // SAFETY: the encoder is live and `params` is fully set up and outlives the call.
        if unsafe { initialize(this.encoder.as_ptr(), &params) } != OK {
            return Err(MediaError::Unavailable("OpenH264 encoder settings"));
        }
        let mut quiet: c_int = WELS_LOG_QUIET as c_int;
        let mut format: c_int = videoFormatI420 as c_int;
        let set_option = vt.SetOption.ok_or(MediaError::Encoder("vtable"))?;
        // SAFETY: both options take a pointer to an `int`, which lives for each call.
        let ok = unsafe {
            set_option(
                this.encoder.as_ptr(),
                ENCODER_OPTION_TRACE_LEVEL,
                (&raw mut quiet).cast::<c_void>(),
            ) == OK
                && set_option(
                    this.encoder.as_ptr(),
                    ENCODER_OPTION_DATAFORMAT,
                    (&raw mut format).cast::<c_void>(),
                ) == OK
        };
        if !ok {
            return Err(MediaError::Encoder("options"));
        }
        Ok(this)
    }

    fn vtable(&self) -> Result<&'static ISVCEncoderVtbl, MediaError> {
        // SAFETY: a live encoder points at its vtable pointer, and the vtable is static data in
        // OpenH264 that outlives every encoder.
        unsafe { (*self.encoder.as_ptr()).as_ref() }.ok_or(MediaError::Encoder("vtable"))
    }
}

fn int(value: u32, what: &'static str) -> Result<c_int, MediaError> {
    c_int::try_from(value).map_err(|_| MediaError::BadConfig(what))
}

/// Low latency, constant bitrate, one spatial layer at the rung's size.
fn configure(params: &mut SEncParamExt, rung: &Rung, fps: u32, levels: u8) -> Result<(), MediaError> {
    let (width, height) = (c_int::from(rung.width), c_int::from(rung.height));
    let bitrate = int(rung.bitrate_bps, "bitrate")?;
    let rate = fps.max(1) as f32;
    params.iUsageType = CAMERA_VIDEO_REAL_TIME;
    params.iPicWidth = width;
    params.iPicHeight = height;
    params.iTargetBitrate = bitrate;
    params.iRCMode = RC_BITRATE_MODE;
    params.fMaxFrameRate = rate;
    params.iTemporalLayerNum = c_int::from(levels);
    params.iSpatialLayerNum = 1;
    params.iComplexityMode = LOW_COMPLEXITY;
    // 0: no IDR unless forced.
    params.uiIntraPeriod = 0;
    params.eSpsPpsIdStrategy = CONSTANT_ID;
    params.bPrefixNalAddingCtrl = true;
    params.bEnableSSEI = false;
    params.iEntropyCodingModeFlag = 0;
    params.bEnableFrameSkip = false;
    params.iMultipleThreadIdc = 1;
    params.bEnableDenoise = false;
    params.bEnableSceneChangeDetect = false;
    params.bEnableBackgroundDetection = false;
    params.bEnableLongTermReference = false;
    let layer = &mut params.sSpatialLayers[0];
    layer.iVideoWidth = width;
    layer.iVideoHeight = height;
    layer.fFrameRate = rate;
    layer.iSpatialBitrate = bitrate;
    layer.iMaxSpatialBitrate = bitrate;
    layer.uiProfileIdc = PRO_BASELINE;
    layer.sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
    layer.sSliceArgument.uiSliceNum = 1;
    Ok(())
}

impl RungEncoder for OpenH264Rung {
    fn encode(&mut self, nv12: &[u8], timestamp_us: u64, keyframe: bool) -> Result<Vec<u8>, MediaError> {
        let (w, h) = (usize::from(self.rung.width), usize::from(self.rung.height));
        nv12_to_i420(nv12, self.rung.width, self.rung.height, &mut self.i420)?;
        let vt = self.vtable()?;
        if keyframe {
            let force = vt.ForceIntraFrame.ok_or(MediaError::Encoder("vtable"))?;
            // SAFETY: the encoder is live; the call takes no pointers.
            if unsafe { force(self.encoder.as_ptr(), true) } != OK {
                return Err(MediaError::Encoder("force keyframe"));
            }
        }
        let (y_len, c_len) = (w * h, w * h / 4);
        let base = self.i420.as_mut_ptr();
        let half = int(u32::from(self.rung.width / 2), "width")?;
        let picture = SSourcePicture {
            iColorFormat: videoFormatI420 as c_int,
            iStride: [c_int::from(self.rung.width), half, half, 0],
            // SAFETY: `i420` holds y_len + 2 * c_len bytes (checked by nv12_to_i420), so both
            // offsets stay inside it.
            pData: unsafe { [base, base.add(y_len), base.add(y_len + c_len), null_mut()] },
            iPicWidth: c_int::from(self.rung.width),
            iPicHeight: c_int::from(self.rung.height),
            uiTimeStamp: i64::try_from(timestamp_us / 1_000).unwrap_or(i64::MAX),
            ..SSourcePicture::default()
        };
        let encode = vt.EncodeFrame.ok_or(MediaError::Encoder("vtable"))?;
        // SAFETY: the encoder is live; `picture` points into `i420`, which holds the whole
        // picture at the strides given and is not touched during the call; `info` is a valid
        // output struct.
        if unsafe { encode(self.encoder.as_ptr(), &picture, &mut *self.info) } != OK {
            return Err(MediaError::Encoder("encode"));
        }
        let kind = self.info.eFrameType;
        if kind == videoFrameTypeSkip || kind == videoFrameTypeInvalid {
            return Err(MediaError::Encoder("encoder skipped a picture"));
        }
        let layers = usize::try_from(self.info.iLayerNum).unwrap_or(0);
        let mut out = Vec::new();
        for layer in self.info.sLayerInfo.iter().take(layers) {
            let count = usize::try_from(layer.iNalCount).unwrap_or(0);
            if count == 0 || layer.pNalLengthInByte.is_null() || layer.pBsBuf.is_null() {
                continue;
            }
            // SAFETY: OpenH264 sets `pNalLengthInByte` to `iNalCount` lengths for this layer,
            // valid until the next encode call.
            let lengths = unsafe { std::slice::from_raw_parts(layer.pNalLengthInByte, count) };
            let total = lengths
                .iter()
                .try_fold(0usize, |sum, len| sum.checked_add(usize::try_from(*len).ok()?))
                .ok_or(MediaError::Encoder("unit length"))?;
            // SAFETY: the layer's units lie back to back in `pBsBuf`, `total` bytes in all,
            // valid until the next encode call; they are copied out at once.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(layer.pBsBuf, total) });
        }
        if out.is_empty() {
            return Err(MediaError::Encoder("no output"));
        }
        Ok(out)
    }

    fn restart(&mut self) -> Result<(), MediaError> {
        *self = Self::open(&self.rung, self.fps)?;
        Ok(())
    }
}

impl Drop for OpenH264Rung {
    fn drop(&mut self) {
        if let Ok(vt) = self.vtable()
            && let Some(uninitialize) = vt.Uninitialize
        {
            // SAFETY: the encoder is live; Uninitialize is safe on any created encoder.
            unsafe { uninitialize(self.encoder.as_ptr()) };
        }
        // SAFETY: created by WelsCreateSVCEncoder and destroyed exactly once, here.
        unsafe { APILoader::WelsDestroySVCEncoder(self.encoder.as_ptr()) };
    }
}

/// An H.264 decoder for one sender's spatial layer.
pub struct OpenH264Decoder {
    decoder: NonNull<ISVCDecoder>,
    picture: Vec<u8>,
}

impl OpenH264Decoder {
    pub fn open() -> Result<Self, MediaError> {
        let mut raw: *mut ISVCDecoder = null_mut();
        // SAFETY: `raw` is a valid out pointer for the call.
        let rc = unsafe { APILoader::WelsCreateDecoder(&mut raw) };
        let decoder = NonNull::new(raw).filter(|_| rc == 0);
        let decoder = decoder.ok_or(MediaError::Unavailable("OpenH264 decoder"))?;
        let this = Self {
            decoder,
            picture: Vec::new(),
        };
        let vt = this.vtable()?;
        let mut params = SDecodingParam::default();
        params.uiTargetDqLayer = u8::MAX;
        params.eEcActiveIdc = ERROR_CON_DISABLE;
        params.sVideoProperty.size = std::mem::size_of_val(&params.sVideoProperty) as u32;
        params.sVideoProperty.eVideoBsType = VIDEO_BITSTREAM_DEFAULT;
        let initialize = vt.Initialize.ok_or(MediaError::Decoder("vtable"))?;
        // SAFETY: the decoder is live and `params` is fully set up and outlives the call.
        if unsafe { initialize(this.decoder.as_ptr(), &params) } != 0 {
            return Err(MediaError::Unavailable("OpenH264 decoder settings"));
        }
        let mut quiet: c_int = WELS_LOG_QUIET as c_int;
        let set_option = vt.SetOption.ok_or(MediaError::Decoder("vtable"))?;
        // SAFETY: the option takes a pointer to an `int`, which lives for the call.
        let rc = unsafe {
            set_option(
                this.decoder.as_ptr(),
                DECODER_OPTION_TRACE_LEVEL,
                (&raw mut quiet).cast::<c_void>(),
            )
        };
        if rc != 0 {
            return Err(MediaError::Decoder("options"));
        }
        Ok(this)
    }

    fn vtable(&self) -> Result<&'static ISVCDecoderVtbl, MediaError> {
        // SAFETY: a live decoder points at its vtable pointer, static data in OpenH264.
        unsafe { (*self.decoder.as_ptr()).as_ref() }.ok_or(MediaError::Decoder("vtable"))
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn decode(&mut self, bitstream: &[u8], timestamp_us: u64) -> Result<Option<RawPicture<'_>>, MediaError> {
        if bitstream.is_empty() {
            return Err(MediaError::Malformed("empty access unit"));
        }
        let len = c_int::try_from(bitstream.len()).map_err(|_| MediaError::TooLarge)?;
        let vt = self.vtable()?;
        let decode = vt.DecodeFrameNoDelay.ok_or(MediaError::Decoder("vtable"))?;
        let mut planes: [*mut u8; 3] = [null_mut(); 3];
        let mut info = SBufferInfo::default();
        // SAFETY: the decoder is live; `bitstream` is `len` readable bytes; `planes` and `info`
        // are valid outputs. OpenH264 only reads the input.
        let state = unsafe {
            decode(
                self.decoder.as_ptr(),
                bitstream.as_ptr(),
                len,
                planes.as_mut_ptr(),
                &mut info,
            )
        };
        if state != dsErrorFree {
            return Err(MediaError::Decoder("bitstream"));
        }
        if info.iBufferStatus != 1 || planes.iter().any(|p| p.is_null()) {
            return Ok(None);
        }
        // SAFETY: with iBufferStatus 1 the union holds the system buffer description.
        let layout = unsafe { info.UsrData.sSystemBuffer };
        let width = u32::try_from(layout.iWidth).map_err(|_| MediaError::Decoder("picture size"))?;
        let height = u32::try_from(layout.iHeight).map_err(|_| MediaError::Decoder("picture size"))?;
        let (w, h) = check_size(width, height)?;
        let pitch = |i: usize| usize::try_from(layout.iStride[i]).map_err(|_| MediaError::Decoder("pitch"));
        let (y_pitch, c_pitch) = (pitch(0)?, pitch(1)?);
        let (rows, c_rows) = (usize::from(h), usize::from(h) / 2);
        // SAFETY: OpenH264 hands back a luma plane of `height` rows at `iStride[0]` and two
        // chroma planes of `height / 2` rows at `iStride[1]`, valid until the next call on this
        // decoder. They are copied into `self.picture` before this returns.
        let (y, u, v) = unsafe {
            (
                std::slice::from_raw_parts(planes[0], y_pitch * rows),
                std::slice::from_raw_parts(planes[1], c_pitch * c_rows),
                std::slice::from_raw_parts(planes[2], c_pitch * c_rows),
            )
        };
        i420_to_nv12(
            Plane { data: y, pitch: y_pitch },
            Plane { data: u, pitch: c_pitch },
            Plane { data: v, pitch: c_pitch },
            w,
            h,
            &mut self.picture,
        )
        .map_err(|_| MediaError::Decoder("picture layout"))?;
        RawPicture::new(w, h, usize::from(w), timestamp_us, &self.picture).map(Some)
    }
}

impl Drop for OpenH264Decoder {
    fn drop(&mut self) {
        if let Ok(vt) = self.vtable()
            && let Some(uninitialize) = vt.Uninitialize
        {
            // SAFETY: the decoder is live; Uninitialize is safe on any created decoder.
            unsafe { uninitialize(self.decoder.as_ptr()) };
        }
        // SAFETY: created by WelsCreateDecoder and destroyed exactly once, here.
        unsafe { APILoader::WelsDestroyDecoder(self.decoder.as_ptr()) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::encoder::VideoEncoder;
    use crate::ladder::{Ladder, Tier};
    use crate::schedule::Schedule;

    /// A grey test picture with a moving bar, packed NV12.
    fn picture(width: u16, height: u16, frame: usize) -> Vec<u8> {
        let (w, h) = (usize::from(width), usize::from(height));
        let mut data = vec![128u8; w * h * 3 / 2];
        for row in 0..h {
            let x = (frame * 8 + row) % w;
            data[row * w + x] = 235;
        }
        data
    }

    fn small_ladder() -> Ladder {
        let rung = |tier, width, height, top_temporal| Rung {
            tier,
            width,
            height,
            top_temporal,
            bitrate_bps: 300_000,
        };
        Ladder::new(
            30,
            2,
            vec![rung(Tier::Thumb, 160, 90, 0), rung(Tier::Hd, 320, 180, 1)],
        )
        .unwrap()
    }

    #[test]
    fn encodes_and_decodes_every_planned_layer() {
        let ladder = small_ladder();
        let top = ladder.rungs().last().copied().unwrap();
        let mut encoder = openh264(&ladder).unwrap();
        let mut decoders: Vec<OpenH264Decoder> =
            ladder.rungs().iter().map(|_| OpenH264Decoder::open().unwrap()).collect();
        let mut schedule = Schedule::new(ladder.clone());
        let mut decoded = 0;
        for frame in 0..12 {
            let data = picture(top.width, top.height, frame);
            let plan = schedule.next(Instant::now());
            let ts = frame as u64 * 33_333;
            let raw = RawPicture::new(top.width, top.height, usize::from(top.width), ts, &data).unwrap();
            for layer in encoder.encode(&raw, &plan).unwrap() {
                let decoder = &mut decoders[usize::from(layer.layer.spatial)];
                if let Some(out) = decoder.decode(&layer.bitstream, ts).unwrap() {
                    assert_eq!((out.width, out.height), (layer.width, layer.height));
                    decoded += 1;
                }
            }
        }
        assert!(decoded > 0);
    }
}
