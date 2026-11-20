//! Your video, encoded: a thread that turns captured pictures into H.264 layers.
//!
//! Purpose: one thread per [`Stream`]. The camera or screen thread hands over each picture
//! ([`Captured`]); this thread fits a ladder to its size ([`ladder_for`], [`screen_ladder`]),
//! plans each picture with a [`Schedule`], encodes
//! it with Media Foundation (GPU first), and writes a [`VideoHeader`] in front of each layer.
//! The session task seals and sends what comes out.
//!
//! Invariants:
//! - The encoder lives and dies on this thread; Media Foundation objects never cross threads.
//! - While nobody else is in the call nothing is encoded, and the first picture after that is
//!   a keyframe.
//! - A failed encoder is dropped and reopened at most every few seconds; the call goes on
//!   without your video meanwhile.
//! - Keyframe requests set a flag; [`Schedule`] limits how often they are obeyed.
//! - Picture numbers keep rising across encoders. A new size (a shared area chosen or
//!   changed, another camera), a new data use choice or a failure reopens the encoder, and
//!   its schedule continues the old numbering: receivers drop pictures numbered at or below
//!   the last they decoded, so restarting from 0 froze the picture for those watching.
//! - A shared screen is one layer, marked as spatial 0 and temporal 0, so a forwarder passes it
//!   to everyone whatever size they ask for. Only the sealed header says it is a screen.
//! - The ladder follows the data use choice (`data_use.rs`): a new choice reopens the encoder
//!   with a keyframe, and Audio only encodes no camera at all. Auto on a slow link counts as
//!   Data saver (`DataUseCell::effective`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use veil_media::ladder::{Rung, Tier};
use veil_media::{Ladder, MediaError, RawPicture, Schedule, VideoEncoder, VideoHeader};
use veil_security::layer::Layer;

use super::Captured;
use crate::data_use::{DataUse, DataUseCell};
use crate::media::{Worker, lock};

/// How long to wait before trying an encoder again after it failed.
const RETRY: Duration = Duration::from_secs(5);
/// The thumbnail's width; its height keeps the camera's shape.
const THUMB_WIDTH: u32 = 640;
/// The largest layer sent: 1080p.
const TOP_WIDTH: u32 = 1_920;
/// The longest side of a shared screen as sent.
/// shortcut: 1080p-class, so text on a larger screen is scaled. Raise it with a higher-rate
/// capture path and a bitrate that follows the link.
const SCREEN_EDGE: u32 = 1_920;
const SCREEN_BITRATE: u32 = 4_000_000;
/// The camera's one layer in Data saver: its width, and its rate at half the capture rate.
const SAVER_WIDTH: u32 = 320;
const SAVER_BITRATE: u32 = 150_000;
/// A shared screen in Data saver and Audio only: its longest side and its rate.
const SAVER_SCREEN_EDGE: u32 = 1_280;
const SAVER_SCREEN_BITRATE: u32 = 600_000;

/// Which of your two video streams a thread encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Camera,
    Screen,
}

/// Flags the session task sets for this thread.
#[derive(Default)]
pub struct VideoControl {
    /// Someone asked for a keyframe, or a new member linked up.
    pub keyframe: AtomicBool,
    /// Someone else is in the call.
    pub watching: AtomicBool,
    /// Why there is no encoder, for the log.
    pub error: Mutex<Option<String>>,
}

/// One encoded layer, its video header in front, ready to seal with `layer`.
pub struct Encoded {
    pub layer: Layer,
    pub payload: Vec<u8>,
}

/// A ladder for a camera of `width` x `height` at `fps`: a 640-wide thumbnail at half rate,
/// and the camera's own size (at most 1080p) at full rate. A small camera gets one layer.
/// Data saver gets one 320-wide layer at half rate.
pub fn ladder_for(
    width: u16,
    height: u16,
    fps: u32,
    data_use: DataUse,
) -> Result<Ladder, MediaError> {
    let (w, h) = (u32::from(width), u32::from(height));
    if w == 0 || h == 0 {
        return Err(MediaError::BadConfig("camera size"));
    }
    let fit = |target: u32| -> (u16, u16) {
        let target = target.min(w);
        let scaled = (h * target / w) & !1;
        (
            u16::try_from(target & !1).unwrap_or(u16::MAX),
            u16::try_from(scaled.max(2)).unwrap_or(u16::MAX),
        )
    };
    let top = fit(TOP_WIDTH);
    let top_bitrate = bitrate(top);
    let rungs = if data_use != DataUse::Auto {
        vec![rung(Tier::Thumb, fit(SAVER_WIDTH), 0, SAVER_BITRATE)]
    } else if w <= THUMB_WIDTH {
        vec![rung(Tier::Thumb, top, 1, top_bitrate)]
    } else {
        vec![
            rung(Tier::Thumb, fit(THUMB_WIDTH), 0, 500_000),
            rung(Tier::Hd, top, 1, top_bitrate),
        ]
    };
    Ladder::new(fps.clamp(1, 60), 2, rungs)
}

/// One layer for a shared `width` x `height` picture, at most [`SCREEN_EDGE`] on its longest
/// side (or [`SAVER_SCREEN_EDGE`] when saving data), keeping its shape.
pub fn screen_ladder(
    width: u16,
    height: u16,
    fps: u32,
    data_use: DataUse,
) -> Result<Ladder, MediaError> {
    let (w, h) = (u32::from(width), u32::from(height));
    if w == 0 || h == 0 {
        return Err(MediaError::BadConfig("screen size"));
    }
    if data_use != DataUse::Auto {
        let longest = w.max(h);
        let fit = |side: u32| (side * SAVER_SCREEN_EDGE.min(longest) / longest).max(2) & !1;
        let size = (
            u16::try_from(fit(w)).unwrap_or(u16::MAX),
            u16::try_from(fit(h)).unwrap_or(u16::MAX),
        );
        let rungs = vec![rung(Tier::Thumb, size, 0, SAVER_SCREEN_BITRATE)];
        return Ladder::new(fps.clamp(1, 60), 1, rungs);
    }
    let longest = w.max(h);
    let fit = |side: u32| (side * SCREEN_EDGE.min(longest) / longest).max(2) & !1;
    let size = (
        u16::try_from(fit(w)).unwrap_or(u16::MAX),
        u16::try_from(fit(h)).unwrap_or(u16::MAX),
    );
    let area = u64::from(size.0) * u64::from(size.1);
    let bps = u64::from(SCREEN_BITRATE) * area / (1_920 * 1_080);
    let bitrate = u32::try_from(bps.clamp(1_000_000, u64::from(SCREEN_BITRATE)))
        .unwrap_or(SCREEN_BITRATE);
    Ladder::new(fps.clamp(1, 60), 1, vec![rung(Tier::Thumb, size, 0, bitrate)])
}

/// The architecture's 3 Mbit/s for 1080p, scaled by picture area, at least 0.5 Mbit/s.
fn bitrate((w, h): (u16, u16)) -> u32 {
    let area = u64::from(w) * u64::from(h);
    let bps = 3_000_000 * area / (1_920 * 1_080);
    u32::try_from(bps.clamp(500_000, 3_000_000)).unwrap_or(3_000_000)
}

fn rung(tier: Tier, (width, height): (u16, u16), top_temporal: u8, bitrate_bps: u32) -> Rung {
    Rung {
        tier,
        width,
        height,
        top_temporal,
        bitrate_bps,
    }
}

pub fn spawn(
    pictures: Receiver<Captured>,
    out: mpsc::Sender<Encoded>,
    control: Arc<VideoControl>,
    stream: Stream,
    data: Arc<DataUseCell>,
) -> Worker {
    Worker::spawn(move |stop| run(&pictures, &out, &control, stream, &data, &stop))
}

struct Active {
    size: (u16, u16, u32),
    data_use: DataUse,
    schedule: Schedule,
    encoder: Box<dyn VideoEncoder>,
}

fn run(
    pictures: &Receiver<Captured>,
    out: &mpsc::Sender<Encoded>,
    control: &VideoControl,
    stream: Stream,
    data: &DataUseCell,
    stop: &AtomicBool,
) {
    let mut active: Option<Active> = None;
    let mut retry_at = Instant::now();
    let mut resumed = true;
    // Carried across encoders, so a reopened one goes on numbering where the last stopped.
    let mut next_picture = 0;
    while !stop.load(Ordering::Relaxed) {
        let picture = match pictures.recv_timeout(Duration::from_millis(100)) {
            Ok(picture) => picture,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let data_use = data.effective();
        let paused = stream == Stream::Camera && !data_use.sends_camera();
        if paused || !control.watching.load(Ordering::Relaxed) {
            // Closed while paused, so the encoder is not held for nothing.
            if paused {
                active = None;
            }
            resumed = true;
            continue;
        }
        let size = (picture.width, picture.height, picture.fps);
        if active.as_ref().is_some_and(|a| a.data_use != data_use) {
            active = None;
            resumed = true;
        }
        if active.as_ref().is_some_and(|a| a.size != size) {
            active = None;
        }
        if active.is_none() {
            if Instant::now() < retry_at {
                continue;
            }
            match open(size, stream, data_use, next_picture) {
                Ok(opened) => active = Some(opened),
                Err(e) => {
                    *lock(&control.error) = Some(e.to_string());
                    crate::note!("no video encoder: {e}");
                    retry_at = Instant::now() + RETRY;
                    continue;
                }
            }
        }
        let Some(current) = &mut active else { continue };
        if control.keyframe.swap(false, Ordering::Relaxed) || resumed {
            current.schedule.request_keyframe();
            resumed = false;
        }
        let encoded = encode(current, &picture, stream);
        next_picture = current.schedule.next_picture();
        match encoded {
            Ok(layers) => {
                for layer in layers {
                    // A full queue means the network is behind: drop, the gate recovers.
                    let _ = out.try_send(layer);
                }
            }
            Err(e) => {
                crate::note!("video encoder failed, reopening: {e}");
                active = None;
                retry_at = Instant::now() + RETRY;
                resumed = true;
            }
        }
    }
}

fn open(
    size: (u16, u16, u32),
    stream: Stream,
    data_use: DataUse,
    first_picture: u64,
) -> Result<Active, MediaError> {
    let ladder = match stream {
        Stream::Camera => ladder_for(size.0, size.1, size.2, data_use)?,
        Stream::Screen => screen_ladder(size.0, size.1, size.2, data_use)?,
    };
    let encoder = platform_encoder(&ladder)?;
    Ok(Active {
        size,
        data_use,
        schedule: Schedule::continuing(ladder, first_picture),
        encoder,
    })
}

fn platform_encoder(ladder: &Ladder) -> Result<Box<dyn VideoEncoder>, MediaError> {
    veil_media::platform::video_encoder(ladder)
}

fn encode(
    active: &mut Active,
    picture: &Captured,
    stream: Stream,
) -> Result<Vec<Encoded>, MediaError> {
    let plan = active.schedule.next(Instant::now());
    if plan.layers.is_empty() {
        return Ok(Vec::new());
    }
    let raw = RawPicture::new(
        picture.width,
        picture.height,
        picture.stride,
        picture.timestamp_us,
        &picture.data,
    )?;
    let codec = active.encoder.codec();
    let layers = active.encoder.encode(&raw, &plan)?;
    layers
        .into_iter()
        .map(|layer| {
            let header = VideoHeader {
                codec,
                picture: plan.picture,
                reference: plan.reference,
                needs_lower: layer.needs_lower,
                screen: stream == Stream::Screen,
                timestamp_us: raw.timestamp_us,
                width: layer.width,
                height: layer.height,
            };
            Ok(Encoded {
                layer: layer.layer,
                payload: header.write(&layer.bitstream)?,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sizes(ladder: &Ladder) -> Vec<(u16, u16, u8)> {
        ladder
            .rungs()
            .iter()
            .map(|r| (r.width, r.height, r.top_temporal))
            .collect()
    }

    #[test]
    fn a_screen_is_one_layer_that_keeps_its_shape() {
        let wide = screen_ladder(3_840, 2_160, 10, DataUse::Auto).unwrap();
        assert_eq!(sizes(&wide), [(1_920, 1_080, 0)]);
        assert_eq!(wide.rungs()[0].tier.spatial(), 0);
        assert_eq!(wide.rungs()[0].bitrate_bps, SCREEN_BITRATE);
        let tall = screen_ladder(1_440, 2_560, 10, DataUse::Auto).unwrap();
        assert_eq!(sizes(&tall), [(1_080, 1_920, 0)]);
        let small = screen_ladder(800, 600, 10, DataUse::Auto).unwrap();
        assert_eq!(sizes(&small), [(800, 600, 0)]);
        assert_eq!(small.rungs()[0].bitrate_bps, 1_000_000);
        assert!(screen_ladder(0, 600, 10, DataUse::Auto).is_err());
    }

    #[test]
    fn a_720p_camera_sends_a_thumbnail_and_its_own_size() {
        let ladder = ladder_for(1_280, 720, 30, DataUse::Auto).unwrap();
        assert_eq!(sizes(&ladder), [(640, 360, 0), (1_280, 720, 1)]);
        assert_eq!(ladder.capture_fps(), 30);
        assert_eq!(ladder.fps(&ladder.rungs()[0]), 15);
        assert_eq!(ladder.fps(&ladder.rungs()[1]), 30);
    }

    #[test]
    fn a_4k_camera_is_capped_at_1080p_and_a_small_one_sends_one_layer() {
        let big = ladder_for(3_840, 2_160, 60, DataUse::Auto).unwrap();
        assert_eq!(sizes(&big)[1], (1_920, 1_080, 1));
        assert_eq!(big.rungs()[1].bitrate_bps, 3_000_000);
        let small = ladder_for(640, 480, 30, DataUse::Auto).unwrap();
        assert_eq!(sizes(&small), [(640, 480, 1)]);
        let odd = ladder_for(1_000, 750, 30, DataUse::Auto).unwrap();
        assert_eq!(sizes(&odd)[0], (640, 480, 0));
        assert!(ladder_for(0, 720, 30, DataUse::Auto).is_err());
    }

    #[test]
    fn data_saver_sends_one_small_slow_layer() {
        let ladder = ladder_for(1_280, 720, 30, DataUse::Saver).unwrap();
        assert_eq!(sizes(&ladder), [(320, 180, 0)]);
        assert_eq!(ladder.rungs()[0].bitrate_bps, SAVER_BITRATE);
        assert_eq!(ladder.fps(&ladder.rungs()[0]), 15);
        let screen = screen_ladder(3_840, 2_160, 10, DataUse::Saver).unwrap();
        assert_eq!(sizes(&screen), [(1_280, 720, 0)]);
        assert_eq!(screen.rungs()[0].bitrate_bps, SAVER_SCREEN_BITRATE);
        let audio_only = screen_ladder(800, 600, 10, DataUse::AudioOnly).unwrap();
        assert_eq!(sizes(&audio_only), [(800, 600, 0)]);
    }
}
