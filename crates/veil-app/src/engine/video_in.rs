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
//! - A shared screen keeps its full size, so its text stays readable.

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
    keyframes: mpsc::UnboundedSender<u32>,
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
    keyframes: &mpsc::UnboundedSender<u32>,
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
                let _ = keyframes.send(frame.id);
                continue;
            }
        }
        match decode(source, bitstream, &header) {
            Ok(Some((rgba, size))) => shared.put_picture(picture_key(stream), rgba, size),
            Ok(None) => {}
            Err(e) => {
                crate::note!("video decoder failed: {e}");
                source.decoder = None;
                source.gate = Gate::default();
                let _ = keyframes.send(frame.id);
            }
        }
        forget_quiet(&mut sources, shared);
    }
}

/// A decoded picture for the screen, and the size it was decoded at (before any halving).
type Decoded = (crate::convert::Rgba, (u16, u16));

fn decode(
    source: &mut Source,
    bitstream: &[u8],
    header: &VideoHeader,
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
    // Tiles are rarely wider than half a 1080p picture; halving costs a quarter of the work.
    Ok(
        crate::convert::nv12_to_rgba(picture.data, w, h, picture.stride, w > 960 && !header.screen)
            .map(|rgba| (rgba, size)),
    )
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
