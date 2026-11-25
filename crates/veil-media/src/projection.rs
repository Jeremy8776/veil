//! Android screen share (SHARE-02): the checks on what Java's MediaProjection hands to Rust.
//!
//! Purpose: Java (`ScreenShare.java`) owns the projection, its virtual display, the image
//! reader and playback capture. Rust polls them by generation through `ffi/android_share.rs`.
//! This module holds the parts that do not need a phone, so they are tested on every desktop:
//! unpacking a copied picture, turning copied sound into samples, and counting who still
//! needs the projection.
//!
//! Invariants:
//! - A picture or sound buffer from Java is a copy. It is checked against its header and a
//!   size cap before use; anything malformed is refused, never read past.
//! - Each consent prompt gets a new generation. A hold or release for an older generation
//!   changes nothing, so a late caller cannot stop or keep a newer share.
//! - The projection stops when its last holder lets go: the chooser's answer, each capture
//!   and the share sound each hold it once.

use crate::MediaError;

/// The longest side of a shared phone picture. Java scales the virtual display to fit.
pub const MAX_SIDE: u32 = 1_920;
/// The most pixels in one shared phone picture: 1920 x 1080.
pub const MAX_PIXELS: usize = 1_920 * 1_080;
/// Bytes before the pixels: width and height, each a little-endian `u16`.
pub const HEADER: usize = 4;
/// The largest picture buffer Rust accepts from Java.
pub const MAX_FRAME_BYTES: usize = HEADER + MAX_PIXELS * 4;
/// The most sound Rust accepts in one read: 80 ms of 16-bit stereo at 48 kHz.
pub const MAX_SOUND_BYTES: usize = 48 * 80 * 2 * 2;

/// Where a share's consent and capture stand, as Java reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The system's consent prompt is open.
    Waiting,
    /// Consent was given and the projection is running.
    Live,
    /// The person said no, or Android refused to start the projection.
    Denied,
    /// The projection has stopped: Stop was pressed, consent was revoked, the screen locked,
    /// or a newer share replaced it.
    Ended,
}

impl Status {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Waiting,
            1 => Self::Live,
            2 => Self::Denied,
            _ => Self::Ended,
        }
    }
}

/// One shared picture: RGBA, rows packed.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Checks and unpacks a picture Java copied: a 4-byte size header, then packed RGBA rows.
pub fn unpack_frame(mut bytes: Vec<u8>) -> Result<Frame, MediaError> {
    let bad = MediaError::Malformed("Android screen picture");
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(bad);
    }
    let Some(header) = bytes.get(..HEADER) else {
        return Err(bad);
    };
    let width = u32::from(u16::from_le_bytes([header[0], header[1]]));
    let height = u32::from(u16::from_le_bytes([header[2], header[3]]));
    let pixels = width as usize * height as usize;
    let fits = width >= 2 && height >= 2 && width <= MAX_SIDE && height <= MAX_SIDE;
    if !fits || pixels > MAX_PIXELS || bytes.len() != HEADER + pixels * 4 {
        return Err(bad);
    }
    bytes.drain(..HEADER);
    Ok(Frame {
        width,
        height,
        rgba: bytes,
    })
}

/// Appends 16-bit little-endian stereo sound to `out` as samples from -1 to 1. A trailing
/// half sample, or anything past [`MAX_SOUND_BYTES`], is dropped.
pub fn push_pcm16(bytes: &[u8], out: &mut Vec<f32>) {
    let bytes = &bytes[..bytes.len().min(MAX_SOUND_BYTES)];
    let (samples, _) = bytes.as_chunks::<2>();
    out.extend(
        samples
            .iter()
            .map(|s| f32::from(i16::from_le_bytes(*s)) / 32_768.0),
    );
}

/// Who still needs the current projection.
#[derive(Debug, Default)]
pub struct Holds {
    /// The live generation and how many hold it.
    current: Option<(u32, u32)>,
}

impl Holds {
    pub const fn new() -> Self {
        Self { current: None }
    }

    /// Consent for `generation` was given: the chooser's answer holds it once.
    pub fn granted(&mut self, generation: u32) {
        self.current = Some((generation, 1));
    }

    /// The generation in use, if any.
    pub fn current(&self) -> Option<u32> {
        self.current.map(|(generation, _)| generation)
    }

    /// Adds a holder to `generation`. False if it is not the one in use.
    pub fn take(&mut self, generation: u32) -> bool {
        match &mut self.current {
            Some((live, count)) if *live == generation => {
                *count += 1;
                true
            }
            _ => false,
        }
    }

    /// Lets go of one hold on `generation`. True when that was the last one, so the
    /// projection should stop.
    pub fn release(&mut self, generation: u32) -> bool {
        let Some((live, count)) = &mut self.current else {
            return false;
        };
        if *live != generation {
            return false;
        }
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.current = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn packed(width: u16, height: u16, pixels: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend((0..pixels * 4).map(|i| i as u8));
        bytes
    }

    #[test]
    fn a_whole_picture_unpacks_without_its_header() {
        let frame = unpack_frame(packed(4, 2, 8)).unwrap();
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(frame.rgba.len(), 32);
        assert_eq!(&frame.rgba[..4], &[0, 1, 2, 3]);
    }

    #[test]
    fn a_picture_that_does_not_match_its_header_is_refused() {
        assert!(unpack_frame(packed(4, 2, 7)).is_err(), "too short");
        assert!(unpack_frame(packed(4, 2, 9)).is_err(), "too long");
        assert!(unpack_frame(vec![1, 0]).is_err(), "no header");
        assert!(unpack_frame(packed(0, 0, 0)).is_err(), "empty");
    }

    #[test]
    fn an_oversized_picture_is_refused_before_it_is_read() {
        assert!(unpack_frame(packed(4_000, 2, 8_000)).is_err(), "too wide");
        let mut huge = vec![0; MAX_FRAME_BYTES + 1];
        huge[..4].copy_from_slice(&[0x80, 0x07, 0x38, 0x04]);
        assert!(unpack_frame(huge).is_err());
        assert!(unpack_frame(packed(1_920, 1_080, MAX_PIXELS)).is_ok(), "the cap fits");
    }

    #[test]
    fn sound_becomes_samples_and_is_capped() {
        let mut out = Vec::new();
        push_pcm16(&[0x00, 0x80, 0xFF, 0x7F, 0x00, 0x00, 0x01], &mut out);
        assert_eq!(out, [-1.0, 32_767.0 / 32_768.0, 0.0], "half sample dropped");
        out.clear();
        push_pcm16(&vec![0; MAX_SOUND_BYTES * 2], &mut out);
        assert_eq!(out.len(), MAX_SOUND_BYTES / 2);
    }

    #[test]
    fn java_states_map_and_unknown_codes_mean_ended() {
        assert_eq!(Status::from_code(0), Status::Waiting);
        assert_eq!(Status::from_code(1), Status::Live);
        assert_eq!(Status::from_code(2), Status::Denied);
        assert_eq!(Status::from_code(3), Status::Ended);
        assert_eq!(Status::from_code(-7), Status::Ended);
    }

    #[test]
    fn the_projection_stops_when_its_last_holder_lets_go() {
        let mut holds = Holds::default();
        holds.granted(5);
        assert!(holds.take(5), "the capture");
        assert!(holds.take(5), "the sound");
        assert!(!holds.release(5), "the chooser's answer");
        assert!(!holds.release(5), "the sound");
        assert!(holds.release(5), "the capture was last");
        assert_eq!(holds.current(), None);
        assert!(!holds.release(5), "nothing left to stop");
    }

    #[test]
    fn an_older_generation_cannot_keep_or_stop_a_newer_share() {
        let mut holds = Holds::default();
        holds.granted(1);
        holds.granted(2);
        assert!(!holds.take(1));
        assert!(!holds.release(1));
        assert_eq!(holds.current(), Some(2));
        assert!(holds.release(2));
    }
}
