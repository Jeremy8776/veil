//! libopus, built from `vendor/opus` by `build.rs`: one encoder and one decoder per stream.
//!
//! Purpose: the only code that calls libopus. [`crate::audio`] holds the settings and framing;
//! this module turns them into libopus calls and checks every length and return code.
//!
//! Invariants:
//! - Each state pointer comes from `opus_*_create`, is never null once stored, and is freed by
//!   `opus_*_destroy` exactly once, in `Drop`.
//! - Every buffer handed to libopus is a Rust slice whose length, in samples per channel or in
//!   bytes, is the count passed alongside it. libopus writes only within that count.
//! - Every `opus_*_ctl` call passes the argument type its request takes (`opus_int32`).
//! - The sample rate is always 48 kHz.

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int};
use std::ptr::NonNull;

use crate::MediaError;
use crate::audio::{AudioSettings, MAX_FRAME_SAMPLES, MAX_PACKET_LEN, SAMPLE_RATE};

/// libopus's opaque encoder state.
#[repr(C)]
struct RawEncoder {
    _private: [u8; 0],
}

/// libopus's opaque decoder state.
#[repr(C)]
struct RawDecoder {
    _private: [u8; 0],
}

const OPUS_OK: c_int = 0;
const OPUS_APPLICATION_VOIP: c_int = 2048;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_SET_VBR_REQUEST: c_int = 4006;
const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
const OPUS_SET_PACKET_LOSS_PERC_REQUEST: c_int = 4014;
const OPUS_SET_DTX_REQUEST: c_int = 4016;
const OPUS_SET_SIGNAL_REQUEST: c_int = 4024;
const OPUS_SIGNAL_VOICE: i32 = 3001;

// `opus_int32` is `int32_t` on every platform Veil builds for (`opus_types.h`).
unsafe extern "C" {
    fn opus_get_version_string() -> *const c_char;

    fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut RawEncoder;
    fn opus_encode_float(
        st: *mut RawEncoder,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;
    fn opus_encoder_ctl(st: *mut RawEncoder, request: c_int, ...) -> c_int;
    fn opus_encoder_destroy(st: *mut RawEncoder);

    fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut RawDecoder;
    fn opus_decode_float(
        st: *mut RawDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_decoder_destroy(st: *mut RawDecoder);
}

/// The linked libopus version, for example `libopus 1.6.1`.
pub fn opus_version() -> String {
    // SAFETY: libopus returns a pointer to a static, NUL-terminated string that is never freed.
    let raw = unsafe { CStr::from_ptr(opus_get_version_string()) };
    raw.to_string_lossy().into_owned()
}

/// One Opus encoder. Not shared between streams: each keeps its own prediction state.
pub struct OpusEncoder {
    state: NonNull<RawEncoder>,
    /// Interleaved samples (all channels) per call to [`OpusEncoder::encode`].
    frame_len: usize,
    channels: usize,
}

// SAFETY: the encoder state is plain heap memory owned by this value. libopus keeps no
// thread-local or global state for it, so moving it to another thread is sound. `encode` and
// the setters take `&mut self`, so it is never used from two threads at once.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(settings: &AudioSettings) -> Result<Self, MediaError> {
        settings.check()?;
        let channels = settings.channels.count();
        let mut error: c_int = OPUS_OK;
        // SAFETY: 48 kHz, one or two channels and VoIP are all valid arguments, and `error`
        // is a live `c_int` for libopus to write the result code into.
        let raw = unsafe {
            opus_encoder_create(
                SAMPLE_RATE as i32,
                channels as c_int,
                OPUS_APPLICATION_VOIP,
                &raw mut error,
            )
        };
        let state = NonNull::new(raw).ok_or(MediaError::Encoder("opus_encoder_create"))?;
        let mut encoder = Self {
            state,
            frame_len: settings.frame_samples() * channels,
            channels,
        };
        if error != OPUS_OK {
            return Err(MediaError::Encoder("opus_encoder_create"));
        }
        encoder.ctl(OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE)?;
        // Constant bitrate unless asked otherwise: variable packet sizes can leak words.
        encoder.ctl(OPUS_SET_VBR_REQUEST, i32::from(!settings.constant_bitrate))?;
        encoder.ctl(OPUS_SET_COMPLEXITY_REQUEST, i32::from(settings.complexity))?;
        encoder.ctl(OPUS_SET_DTX_REQUEST, i32::from(settings.dtx))?;
        encoder.ctl(OPUS_SET_INBAND_FEC_REQUEST, i32::from(settings.fec))?;
        encoder.set_bitrate(settings.bitrate)?;
        encoder.set_expected_loss(settings.expected_loss_percent)?;
        Ok(encoder)
    }

    /// Interleaved samples per call to [`OpusEncoder::encode`].
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// Encodes one frame of interleaved samples in -1.0..=1.0 into one Opus packet.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, MediaError> {
        if pcm.len() != self.frame_len {
            return Err(MediaError::Malformed("audio frame length"));
        }
        let mut packet = vec![0u8; MAX_PACKET_LEN];
        // SAFETY: `pcm` holds exactly `frame_size * channels` samples and `packet` is
        // `MAX_PACKET_LEN` writable bytes, the two counts passed. The state is live (invariant).
        let written = unsafe {
            opus_encode_float(
                self.state.as_ptr(),
                pcm.as_ptr(),
                (self.frame_len / self.channels) as c_int,
                packet.as_mut_ptr(),
                MAX_PACKET_LEN as i32,
            )
        };
        let written = usize::try_from(written).map_err(|_| MediaError::Encoder("opus_encode"))?;
        if written == 0 || written > MAX_PACKET_LEN {
            return Err(MediaError::Encoder("opus_encode"));
        }
        packet.truncate(written);
        Ok(packet)
    }

    /// Target bitrate in bits per second, 6000 to 510000.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), MediaError> {
        let bitrate = AudioSettings::check_bitrate(bitrate)?;
        self.ctl(OPUS_SET_BITRATE_REQUEST, bitrate)
    }

    /// The loss rate the receivers report, 0 to 100. With FEC on, a higher rate makes the
    /// encoder spend more of the bitrate on redundancy.
    pub fn set_expected_loss(&mut self, percent: u8) -> Result<(), MediaError> {
        if percent > 100 {
            return Err(MediaError::BadConfig("expected loss"));
        }
        self.ctl(OPUS_SET_PACKET_LOSS_PERC_REQUEST, i32::from(percent))
    }

    fn ctl(&mut self, request: c_int, value: i32) -> Result<(), MediaError> {
        // SAFETY: every request this module sends takes one `opus_int32` argument, which is
        // what is passed. The state is live (invariant).
        let code = unsafe { opus_encoder_ctl(self.state.as_ptr(), request, value) };
        if code == OPUS_OK {
            Ok(())
        } else {
            Err(MediaError::BadConfig("opus encoder setting"))
        }
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: the state came from `opus_encoder_create` and is destroyed only here, once.
        unsafe { opus_encoder_destroy(self.state.as_ptr()) }
    }
}

/// One Opus decoder, for one sender's stream.
pub struct OpusDecoder {
    state: NonNull<RawDecoder>,
    channels: usize,
    /// Output buffer, large enough for the longest Opus packet (120 ms).
    pcm: Vec<f32>,
}

// SAFETY: as for `OpusEncoder`: plain heap state owned by this value, used through `&mut self`.
unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    pub fn new(settings: &AudioSettings) -> Result<Self, MediaError> {
        settings.check()?;
        let channels = settings.channels.count();
        let mut error: c_int = OPUS_OK;
        // SAFETY: 48 kHz and one or two channels are valid, and `error` is a live `c_int`.
        let raw =
            unsafe { opus_decoder_create(SAMPLE_RATE as i32, channels as c_int, &raw mut error) };
        let state = NonNull::new(raw).ok_or(MediaError::Decoder("opus_decoder_create"))?;
        let decoder = Self {
            state,
            channels,
            pcm: vec![0.0; MAX_FRAME_SAMPLES * channels],
        };
        if error != OPUS_OK {
            return Err(MediaError::Decoder("opus_decoder_create"));
        }
        Ok(decoder)
    }

    /// Decodes one packet into interleaved samples. A packet that does not parse is an error;
    /// the caller then conceals that frame.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], MediaError> {
        if packet.is_empty() || packet.len() > MAX_PACKET_LEN {
            return Err(MediaError::Malformed("opus packet length"));
        }
        self.run(packet, MAX_FRAME_SAMPLES, false)
    }

    /// Fills one lost frame of `samples` per channel by concealment. `samples` must be a
    /// multiple of 2.5 ms (120 at 48 kHz) and at most 120 ms.
    pub fn conceal(&mut self, samples: usize) -> Result<&[f32], MediaError> {
        check_lost(samples)?;
        self.run(&[], samples, false)
    }

    /// Recovers the lost frame before `next` from the redundancy `next` carries, when the
    /// sender had FEC on. Without redundancy libopus conceals instead, so this always gives
    /// `samples` per channel. Call [`OpusDecoder::decode`] on `next` afterwards as usual.
    pub fn recover(&mut self, next: &[u8], samples: usize) -> Result<&[f32], MediaError> {
        check_lost(samples)?;
        if next.is_empty() || next.len() > MAX_PACKET_LEN {
            return Err(MediaError::Malformed("opus packet length"));
        }
        self.run(next, samples, true)
    }

    fn run(&mut self, packet: &[u8], samples: usize, fec: bool) -> Result<&[f32], MediaError> {
        let data = if packet.is_empty() {
            std::ptr::null()
        } else {
            packet.as_ptr()
        };
        // SAFETY: `data` is null (concealment) or points to `packet.len()` readable bytes, the
        // length passed. `self.pcm` holds `MAX_FRAME_SAMPLES * channels` floats and `samples`
        // is at most `MAX_FRAME_SAMPLES`, so libopus writes within it. The state is live.
        let decoded = unsafe {
            opus_decode_float(
                self.state.as_ptr(),
                data,
                packet.len() as i32,
                self.pcm.as_mut_ptr(),
                samples as c_int,
                c_int::from(fec),
            )
        };
        let decoded = usize::try_from(decoded).map_err(|_| MediaError::Decoder("opus_decode"))?;
        if decoded > samples {
            return Err(MediaError::Decoder("opus_decode"));
        }
        Ok(&self.pcm[..decoded * self.channels])
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: the state came from `opus_decoder_create` and is destroyed only here, once.
        unsafe { opus_decoder_destroy(self.state.as_ptr()) }
    }
}

fn check_lost(samples: usize) -> Result<(), MediaError> {
    if samples == 0 || samples > MAX_FRAME_SAMPLES || !samples.is_multiple_of(120) {
        return Err(MediaError::BadConfig("lost frame length"));
    }
    Ok(())
}
