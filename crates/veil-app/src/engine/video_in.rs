//! Everyone else's video: a thread that checks, decodes and converts each person's pictures.
//!
//! Purpose: the session task hands over each opened frame ([`VideoIn`]). Per person this
//! thread follows one spatial layer (the largest that keeps arriving), lets a [`Gate`] decide
//! whether the frame can decode, decodes it with Media Foundation and converts it to RGBA for
//! that person's tile. When the gate says the stream broke, it asks the session task to
//! request a keyframe from that person.
//!
//! Invariants:
//! - Decoders live and die on this thread, one per stream: a person's camera and their shared
//!   screen are decoded apart, told apart by the sealed video header.
//! - Only one spatial layer is decoded per person; in a mesh every layer arrives, and the
//!   largest is what a tile shows. If the largest stops for a second, the next one down is
//!   used (it then waits for its own keyframe, through the gate).
//! - A decoder error drops that decoder and asks for a keyframe; someone silent for 10 s is
//!   forgotten, and a shared screen after 3 s, which is how a share ends for those watching.
//! - A shared screen keeps its full size, so its text stays readable. A camera picture is
//!   halved only when its tile is no wider than half of it ([`halves`]); halving every
//!   picture over 960 wide showed a 720p camera at 640x360 even in a full-screen tile.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use veil_media::{Gate, MediaError, Verdict, VideoDecoder, VideoHeader};
use veil_security::layer::Layer;

use super::view::{Shared, screen_key};
use crate::media::Worker;

/// The largest layer is given up after this long without a frame of it.
const LAYER_TIMEOUT: Duration = Duration::from_secs(1);
/// Someone is forgotten after this long without any video.
const FORGET_AFTER: Duration = Duration::from_secs(10);
/// A share sends pictures even when the screen is still, so this means it ended.
const SCREEN_FORGET_AFTER: Duration = Duration::from_secs(3);

/// One opened video frame from participant `id`.
pub struct VideoIn {
    pub id: u32,
    pub layer: Layer,
    pub payload: Vec<u8>,
}

pub fn spawn(
    frames: Receiver<VideoIn>,
    keyframes: mpsc::Sender<u32>,
    shared: Arc<Shared>,
) -> Worker {
    Worker::spawn(move |stop| run(&frames, &keyframes, &shared, &stop))
}

/// Which spatial layer to follow for one person.
#[derive(Clone, Copy, Debug)]
struct Follow {
    top: u8,
    top_seen: Instant,
}

impl Follow {
    fn new(now: Instant) -> Self {
        Self {
            top: 0,
            top_seen: now,
        }
    }

    /// Whether to decode a frame of `spatial`: the largest layer seen, or a smaller one once
    /// the largest has stopped.
    fn wants(&mut self, spatial: u8, now: Instant) -> bool {
        let stale = now.saturating_duration_since(self.top_seen) > LAYER_TIMEOUT;
        if spatial >= self.top || stale {
            self.top = spatial;
            self.top_seen = now;
            true
        } else {
            false
        }
    }
}

/// A person's id, and whether this is their shared screen.
type StreamId = (u32, bool);

/// Where the interface finds a stream's pictures.
fn picture_key((id, screen): StreamId) -> u32 {
    if screen { screen_key(id) } else { id }
}

struct Source {
    follow: Follow,
    gate: Gate,
    decoder: Option<Box<dyn VideoDecoder>>,
    last: Instant,
}

fn run(
    frames: &Receiver<VideoIn>,
    keyframes: &mpsc::Sender<u32>,
    shared: &Shared,
    stop: &AtomicBool,
) {
    let mut sources: HashMap<StreamId, Source> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let frame = match frames.recv_timeout(Duration::from_millis(200)) {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => {
                forget_quiet(&mut sources, shared);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        let Ok((header, bitstream)) = VideoHeader::read(&frame.payload) else {
            continue;
        };
        let stream = (frame.id, header.screen);
        let source = sources.entry(stream).or_insert_with(|| Source {
            follow: Follow::new(now),
            gate: Gate::default(),
            decoder: None,
            last: now,
        });
        source.last = now;
        if !source.follow.wants(frame.layer.spatial, now) {
            continue;
        }
        match source.gate.admit(frame.layer, &header) {
            Verdict::Decode => {}
            Verdict::Drop => continue,
            Verdict::DropAndRequestKeyframe => {
                request_keyframe(keyframes, frame.id, &mut source.gate);
                continue;
            }
        }
        let tile = shared.tile_width(frame.id);
        match decode(source, bitstream, &header, tile) {
            Ok(Some((rgba, size))) => shared.put_picture(picture_key(stream), rgba, size),
            Ok(None) => {}
            Err(e) => {
                crate::note!("video decoder failed: {e}");
                source.decoder = None;
                source.gate = Gate::default();
                request_keyframe(keyframes, frame.id, &mut source.gate);
            }
        }
        forget_quiet(&mut sources, shared);
    }
}

/// A full feedback queue must neither block decoding nor permanently consume the gate's
/// one request for this break. Resetting it lets the next undecodable base frame retry.
fn request_keyframe(requests: &mpsc::Sender<u32>, id: u32, gate: &mut Gate) {
    if requests.try_send(id).is_err() { *gate = Gate::default(); }
}

/// A decoded picture for the screen, and the size it was decoded at (before any halving).
type Decoded = (crate::convert::Rgba, (u16, u16));

fn decode(
    source: &mut Source,
    bitstream: &[u8],
    header: &VideoHeader,
    tile: Option<f32>,
) -> Result<Option<Decoded>, MediaError> {
    if source.decoder.is_none() {
        source.decoder = Some(platform_decoder()?);
    }
    let Some(decoder) = &mut source.decoder else {
        return Ok(None);
    };
    let Some(picture) = decoder.decode(bitstream, header.timestamp_us)? else {
        return Ok(None);
    };
    let size = (picture.width, picture.height);
    let (w, h) = (usize::from(picture.width), usize::from(picture.height));
    let halve = halves(picture.width, header.screen, tile);
    Ok(crate::convert::nv12_to_rgba(picture.data, w, h, picture.stride, halve).map(|rgba| (rgba, size)))
}

/// Whether to convert a decoded picture `width` wide at half size, which costs a quarter of
/// the work. Only a camera over 960 wide whose tile (physical pixels; `None` when it is not
/// on screen) is no wider than half the picture, so halving loses nothing visible.
fn halves(width: u16, screen: bool, tile: Option<f32>) -> bool {
    let half = f32::from(width) / 2.0;
    !screen && width > 960 && tile.is_none_or(|tile| tile <= half)
}

fn forget_quiet(sources: &mut HashMap<StreamId, Source>, shared: &Shared) {
    let now = Instant::now();
    sources.retain(|stream, source| {
        let limit = if stream.1 { SCREEN_FORGET_AFTER } else { FORGET_AFTER };
        let keep = now.saturating_duration_since(source.last) < limit;
        if !keep {
            shared.forget_picture(picture_key(*stream));
        }
        keep
    });
}

fn platform_decoder() -> Result<Box<dyn VideoDecoder>, MediaError> {
    veil_media::platform::video_decoder()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_full_keyframe_queue_preserves_the_next_frames_retry() {
        let (requests, mut received) = mpsc::channel(1);
        requests.try_send(1).unwrap();
        let mut gate = Gate::default();
        let layer = veil_security::Layer::new(0, 0, false).unwrap();
        let header = VideoHeader { codec: veil_media::payload::Codec::H264,
            picture: 2, reference: Some(1), needs_lower: false, screen: false,
            timestamp_us: 0, width: 16, height: 16 };
        for _ in 0..1000 {
            assert_eq!(gate.admit(layer, &header), Verdict::DropAndRequestKeyframe);
            request_keyframe(&requests, 2, &mut gate);
        }
        assert_eq!(received.len(), 1);
        assert_eq!(received.try_recv().unwrap(), 1);
        assert_eq!(gate.admit(layer, &header), Verdict::DropAndRequestKeyframe);
        request_keyframe(&requests, 2, &mut gate);
        assert_eq!(received.try_recv().unwrap(), 2);
        assert_eq!(gate.admit(layer, &header), Verdict::Drop, "a delivered request stays coalesced");
    }

    #[test]
    fn a_camera_is_halved_only_for_a_small_tile() {
        assert!(!halves(1_920, false, Some(1_400.0)), "large tile keeps 1080p");
        assert!(!halves(1_280, false, Some(700.0)), "720p in a 700 px tile");
        assert!(halves(1_920, false, Some(960.0)));
        assert!(halves(1_920, false, None), "not on screen");
        assert!(!halves(1_920, true, None), "a screen keeps its size");
        assert!(!halves(960, false, Some(100.0)), "small pictures are never halved");
    }

    #[test]
    fn a_persons_camera_and_screen_are_kept_apart() {
        assert_eq!(picture_key((7, false)), 7);
        assert_ne!(picture_key((7, true)), 7);
        assert_ne!(picture_key((7, true)), picture_key((8, true)));
    }

    #[test]
    fn the_largest_layer_is_followed_and_a_stopped_one_is_given_up() {
        let start = Instant::now();
        let mut follow = Follow::new(start);
        assert!(follow.wants(0, start));
        assert!(follow.wants(1, start), "a larger layer takes over");
        assert!(!follow.wants(0, start), "the smaller one is skipped");
        assert!(follow.wants(1, start + Duration::from_millis(500)));
        let later = start + Duration::from_millis(500) + LAYER_TIMEOUT * 2;
        assert!(follow.wants(0, later), "the larger one stopped");
        assert_eq!(follow.top, 0);
        assert!(follow.wants(1, later), "and it comes back when it returns");
    }
}
