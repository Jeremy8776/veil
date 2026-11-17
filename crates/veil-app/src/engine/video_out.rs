//! Your camera, encoded: a thread that turns captured pictures into H.264 simulcast layers.
//!
//! Purpose: the camera thread hands over each picture ([`Captured`]); this thread fits a
//! ladder to the camera's size ([`ladder_for`]), plans each picture with a [`Schedule`], encodes
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use veil_media::ladder::{Rung, Tier};
use veil_media::{Ladder, MediaError, RawPicture, Schedule, VideoEncoder, VideoHeader};
use veil_security::layer::Layer;

use super::Captured;
use crate::media::{Worker, lock};

/// How long to wait before trying an encoder again after it failed.
const RETRY: Duration = Duration::from_secs(5);
/// The thumbnail's width; its height keeps the camera's shape.
const THUMB_WIDTH: u32 = 640;
/// The largest layer sent: 1080p.
const TOP_WIDTH: u32 = 1_920;

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
pub fn ladder_for(width: u16, height: u16, fps: u32) -> Result<Ladder, MediaError> {
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
    let rungs = if w <= THUMB_WIDTH {
        vec![rung(Tier::Thumb, top, 1, top_bitrate)]
    } else {
        vec![
            rung(Tier::Thumb, fit(THUMB_WIDTH), 0, 500_000),
            rung(Tier::Hd, top, 1, top_bitrate),
        ]
    };
    Ladder::new(fps.clamp(1, 60), 2, rungs)
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
) -> Worker {
    Worker::spawn(move |stop| run(&pictures, &out, &control, &stop))
}

struct Active {
    size: (u16, u16, u32),
    schedule: Schedule,
    encoder: Box<dyn VideoEncoder>,
}

fn run(
    pictures: &Receiver<Captured>,
    out: &mpsc::Sender<Encoded>,
    control: &VideoControl,
    stop: &AtomicBool,
) {
    let mut active: Option<Active> = None;
    let mut retry_at = Instant::now();
    let mut resumed = true;
    while !stop.load(Ordering::Relaxed) {
        let picture = match pictures.recv_timeout(Duration::from_millis(100)) {
            Ok(picture) => picture,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if !control.watching.load(Ordering::Relaxed) {
            resumed = true;
            continue;
        }
        let size = (picture.width, picture.height, picture.fps);
        if active.as_ref().is_some_and(|a| a.size != size) {
            active = None;
        }
        if active.is_none() {
            if Instant::now() < retry_at {
                continue;
            }
            match open(size) {
                Ok(opened) => active = Some(opened),
                Err(e) => {
                    *lock(&control.error) = Some(e.to_string());
                    eprintln!("veil-app: no video encoder: {e}");
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
        match encode(current, &picture) {
            Ok(layers) => {
                for layer in layers {
                    // A full queue means the network is behind: drop, the gate recovers.
                    let _ = out.try_send(layer);
                }
            }
            Err(e) => {
                eprintln!("veil-app: video encoder failed, reopening: {e}");
                active = None;
                retry_at = Instant::now() + RETRY;
                resumed = true;
            }
        }
    }
}

fn open(size: (u16, u16, u32)) -> Result<Active, MediaError> {
    let ladder = ladder_for(size.0, size.1, size.2)?;
    let encoder = platform_encoder(&ladder)?;
    Ok(Active {
        size,
        schedule: Schedule::new(ladder),
        encoder,
    })
}

fn platform_encoder(ladder: &Ladder) -> Result<Box<dyn VideoEncoder>, MediaError> {
    veil_media::platform::video_encoder(ladder)
}

fn encode(active: &mut Active, picture: &Captured) -> Result<Vec<Encoded>, MediaError> {
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
    fn a_720p_camera_sends_a_thumbnail_and_its_own_size() {
        let ladder = ladder_for(1_280, 720, 30).unwrap();
        assert_eq!(sizes(&ladder), [(640, 360, 0), (1_280, 720, 1)]);
        assert_eq!(ladder.capture_fps(), 30);
        assert_eq!(ladder.fps(&ladder.rungs()[0]), 15);
        assert_eq!(ladder.fps(&ladder.rungs()[1]), 30);
    }

    #[test]
    fn a_4k_camera_is_capped_at_1080p_and_a_small_one_sends_one_layer() {
        let big = ladder_for(3_840, 2_160, 60).unwrap();
        assert_eq!(sizes(&big)[1], (1_920, 1_080, 1));
        assert_eq!(big.rungs()[1].bitrate_bps, 3_000_000);
        let small = ladder_for(640, 480, 30).unwrap();
        assert_eq!(sizes(&small), [(640, 480, 1)]);
        let odd = ladder_for(1_000, 750, 30).unwrap();
        assert_eq!(sizes(&odd)[0], (640, 480, 0));
        assert!(ladder_for(0, 720, 30).is_err());
    }
}
