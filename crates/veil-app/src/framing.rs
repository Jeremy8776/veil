//! Face framing: where each person's head sits in their picture, so a tile that crops its
//! picture to fill (the phone's Focus stage, the desktop Focus strip) keeps them in view.
//!
//! Purpose: reuses the on-device person segmenter from Background blur
//! (`veil_media::segment`) on a worker thread. The interface thread only takes a tiny
//! nearest-neighbour sample of a picture at most twice a second per person, and only while a
//! cropping tile asked for framing in the last second.
//!
//! Invariants:
//! - Nothing leaves the device; pictures are sampled in memory and dropped.
//! - Shared screens are never framed; only cameras ([`YOU`] or a participant id).
//! - With no answer yet, or nobody found, the crop stays centred.
//! - The worker never blocks the interface: a full queue drops the sample.
use crate::engine::is_screen;
use eframe::egui;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use veil_media::segment::{MODEL_H, MODEL_W, Segmenter};

/// The key for your own camera.
pub const YOU: u32 = u32::MAX;
/// How often one person's picture is looked at.
const EVERY: Duration = Duration::from_millis(500);
/// How long one request for framing keeps sampling going.
const WANTED_FOR: Duration = Duration::from_secs(1);

type Centres = Arc<Mutex<HashMap<u32, [f32; 2]>>>;

pub struct Framing {
    send: Option<SyncSender<(u32, Vec<f32>)>>,
    centres: Centres,
    last: HashMap<u32, Instant>,
    wanted: Cell<Option<Instant>>,
}

impl Framing {
    pub fn new(ctx: egui::Context) -> Self {
        let centres = Centres::default();
        let (send, receive) = mpsc::sync_channel::<(u32, Vec<f32>)>(2);
        let theirs = Arc::clone(&centres);
        let worker = std::thread::Builder::new()
            .name("veil-framing".into())
            .spawn(move || {
                let mut segmenter = None;
                let mut mask = vec![0u8; MODEL_W * MODEL_H];
                for (key, input) in receive {
                    let s = segmenter.get_or_insert_with(Segmenter::new);
                    s.run(&input, &mut mask);
                    let mut centres = theirs.lock().unwrap_or_else(|e| e.into_inner());
                    match head(&mask, MODEL_W, MODEL_H) {
                        Some(c) => centres.insert(key, c),
                        None => centres.remove(&key),
                    };
                    drop(centres);
                    ctx.request_repaint();
                }
            });
        Self {
            send: worker.is_ok().then_some(send),
            centres,
            last: HashMap::new(),
            wanted: Cell::new(None),
        }
    }

    /// A cropping tile is on screen: keep sampling for a while.
    pub fn want(&self) {
        self.wanted.set(Some(Instant::now()));
    }

    /// Where the head is in `key`'s picture, as fractions of its width and height.
    pub fn centre(&self, key: u32) -> Option<[f32; 2]> {
        let centres = self.centres.lock().unwrap_or_else(|e| e.into_inner());
        centres.get(&key).copied()
    }

    /// Offers a new RGBA picture of `key`; most are skipped.
    pub fn offer(&mut self, key: u32, width: usize, height: usize, rgba: &[u8]) {
        let now = Instant::now();
        let wanted = self
            .wanted
            .get()
            .is_some_and(|at| now.saturating_duration_since(at) < WANTED_FOR);
        if !wanted || (key != YOU && is_screen(key)) {
            return;
        }
        if self
            .last
            .get(&key)
            .is_some_and(|at| now.saturating_duration_since(*at) < EVERY)
        {
            return;
        }
        let (Some(send), Some(input)) = (&self.send, sample(width, height, rgba)) else {
            return;
        };
        if send.try_send((key, input)).is_ok() {
            self.last.insert(key, now);
        }
    }
}

/// The model's planar RGB input in [0, 1], sampled from an RGBA picture of any size.
/// shortcut: a portrait picture is squeezed into the landscape model input, which finds the
/// head less well; letterbox it if portrait framing proves unreliable.
fn sample(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<f32>> {
    if width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return None;
    }
    let plane = MODEL_W * MODEL_H;
    let mut input = vec![0.0; plane * 3];
    for y in 0..MODEL_H {
        let sy = y * height / MODEL_H;
        for x in 0..MODEL_W {
            let at = (sy * width + x * width / MODEL_W) * 4;
            for c in 0..3 {
                input[c * plane + y * MODEL_W + x] = f32::from(rgba[at + c]) / 255.0;
            }
        }
    }
    Some(input)
}

/// The middle of the head in a person mask (255 = person), as fractions of the picture:
/// the weighted centre of the top part of the person. `None` when nobody is in view.
pub fn head(mask: &[u8], w: usize, h: usize) -> Option<[f32; 2]> {
    if w == 0 || h == 0 || mask.len() < w * h {
        return None;
    }
    let row = |y: usize| {
        mask[y * w..][..w]
            .iter()
            .map(|v| u32::from(*v))
            .sum::<u32>()
    };
    // A row counts once about eight full pixels of it are person.
    let enough = 8 * 255;
    let top = (0..h).find(|y| row(*y) >= enough)?;
    let bottom = (0..h).rev().find(|y| row(*y) >= enough)?;
    // The head is the top third or so of what shows of a person.
    let band = ((bottom - top) * 35 / 100).max(h / 12).max(1);
    let (mut sx, mut sy, mut total) = (0.0f64, 0.0f64, 0.0f64);
    for y in top..(top + band).min(h) {
        for (x, v) in mask[y * w..][..w].iter().enumerate() {
            let v = f64::from(*v);
            sx += v * (x as f64 + 0.5);
            sy += v * (y as f64 + 0.5);
            total += v;
        }
    }
    (total > 0.0).then(|| {
        [
            (sx / total / w as f64) as f32,
            (sy / total / h as f64) as f32,
        ]
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A person: a head circle above wide shoulders running off the bottom.
    fn person(w: usize, h: usize, cx: usize) -> Vec<u8> {
        let mut m = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - cx as f32, y as f32 - 40.0);
                let in_head = dx * dx + dy * dy < 400.0;
                let in_body = y > 60 && (x as i32 - cx as i32).abs() < 60;
                if in_head || in_body {
                    m[y * w + x] = 255;
                }
            }
        }
        m
    }

    #[test]
    fn finds_a_head_left_of_centre() {
        let c = head(&person(256, 144, 70), 256, 144).unwrap();
        assert!((c[0] - 70.5 / 256.0).abs() < 0.03, "x {c:?}");
        assert!(c[1] < 0.45, "the head is in the upper part, {c:?}");
    }

    #[test]
    fn nobody_in_view_gives_no_answer() {
        assert!(head(&[0; 256 * 144], 256, 144).is_none());
        assert!(head(&[], 256, 144).is_none());
    }

    #[test]
    fn sampling_any_picture_fills_the_model_input() {
        let rgba = [255u8, 0, 0, 255].repeat(640 * 360);
        let input = sample(640, 360, &rgba).unwrap();
        assert_eq!(input.len(), MODEL_W * MODEL_H * 3);
        assert_eq!(input[0], 1.0);
        assert_eq!(input[MODEL_W * MODEL_H], 0.0);
        assert!(sample(10, 10, &rgba[..8]).is_none());
    }
}
