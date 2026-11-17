//! Background blur for the camera picture: the camera-thread half and the segmentation worker.
//!
//! Purpose: [`BlurEngine::apply`] blurs the background of an NV12 picture in place. It shrinks
//! the picture, hands the small copy to a worker thread that finds the person, and blends the
//! sharp picture over a blurred one with the newest mask it has (see [`grid`]).
//!
//! Invariants:
//! - [`BlurEngine`] is created when blur turns on and dropped when it turns off. It owns one
//!   worker thread; dropping the engine stops and joins it.
//! - The camera thread waits for the worker at most [`MASK_WAIT`] per picture, for the mask of
//!   that same picture, so the outline does not trail a moving person. If the mask is not
//!   ready in time it uses the newest one it has. The two share two single slots, "in" (model
//!   input) and "out" (mask), each behind a Mutex held only to swap buffers. A new input
//!   overwrites an unconsumed one: the newest picture wins, there is no queue.
//! - Buffers are swapped, never copied or reallocated, so a steady state allocates nothing.
//!   Only the first picture of a new size builds the size tables.
//! - The mask is normally the current picture's. Until the first mask arrives the whole picture is
//!   treated as background, so no sharp background leaves while the worker starts. That wait
//!   has no limit: a worker that never answers keeps the picture fully blurred, which is safe.
//! - Once a mask has arrived, a picture more than [`MAX_MASK_AGE`] pictures newer than it is
//!   left unblurred and [`BlurStatus::TooBusy`] is returned. The caller must not send that picture
//!   (`veil-app` closes the camera).
//! - Input is NV12 as in [`crate::capture`]: `stride * height` luma bytes, then `height / 2`
//!   rows of interleaved U,V, each `stride` bytes with `width` of them used. Padding is never
//!   touched. A malformed picture (odd or zero size, `stride < width`, short buffer) is left
//!   untouched and reported as `Blurred`, as there is nothing to retry. Nothing here panics.
//! - Pictures smaller than 256x144 work: they are stretched to the grid and back.
//! - [`Blur::Off`] leaves the picture untouched.

mod grid;
mod kernels;
#[cfg(test)]
mod tests;

use crate::segment::{MODEL_H, MODEL_W, Segmenter};
use grid::{Pipeline, Strength};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How strongly the background is blurred. The interface owns the labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blur {
    #[default]
    Off,
    Light,
    Strong,
}

impl Blur {
    /// Every choice, in picker order.
    pub const ALL: [Blur; 3] = [Blur::Off, Blur::Light, Blur::Strong];
}

/// Pictures a mask may lag behind before blur gives up.
pub const MAX_MASK_AGE: u64 = 3;

/// Longest the camera thread waits for the current picture's mask. The model takes about
/// 3.5 ms at camera pace, so this almost always gets the current mask, while capping the camera
/// thread's cost.
pub const MASK_WAIT: Duration = Duration::from_millis(8);

/// What [`BlurEngine::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurStatus {
    /// The picture was blurred.
    Blurred,
    /// The segmenter cannot keep up; the picture was left as it was.
    TooBusy,
}

const INPUT_LEN: usize = 3 * MODEL_W * MODEL_H;
const MASK_LEN: usize = MODEL_W * MODEL_H;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A worker that panicked mid-swap leaves plain data behind; use it.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct InSlot {
    buf: Vec<f32>,
    seq: u64,
    ready: bool,
    stop: bool,
}

struct OutSlot {
    buf: Vec<u8>,
    seq: u64,
    fresh: bool,
}

struct Shared {
    input: Mutex<InSlot>,
    wake: Condvar,
    out: Mutex<OutSlot>,
    /// Signalled when the worker leaves a new mask in `out`.
    done: Condvar,
}

/// Camera-side blur state plus the segmentation worker thread.
pub struct BlurEngine {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    pipe: Option<Pipeline>,
    /// Newest mask taken from the worker; all zero (background) until the first one.
    mask: Vec<u8>,
    mask_seq: Option<u64>,
    seq: u64,
}

impl BlurEngine {
    /// Starts the worker thread with the real segmenter.
    pub fn start() -> BlurEngine {
        Self::spawn(|| {
            let mut s = Segmenter::new();
            move |input: &[f32], mask: &mut [u8]| s.run(input, mask)
        })
    }

    /// Starts the worker with `segment` in place of the real segmenter (for tests).
    pub fn with_segmenter(segment: impl FnMut(&[f32], &mut [u8]) + Send + 'static) -> BlurEngine {
        Self::spawn(move || segment)
    }

    /// `make` runs on the worker thread, so a slow model load does not delay the caller.
    fn spawn<F, M>(make: M) -> BlurEngine
    where
        F: FnMut(&[f32], &mut [u8]),
        M: FnOnce() -> F + Send + 'static,
    {
        let shared = Arc::new(Shared {
            input: Mutex::new(InSlot {
                buf: vec![0.0; INPUT_LEN],
                seq: 0,
                ready: false,
                stop: false,
            }),
            wake: Condvar::new(),
            out: Mutex::new(OutSlot {
                buf: vec![0; MASK_LEN],
                seq: 0,
                fresh: false,
            }),
            done: Condvar::new(),
        });
        let theirs = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("veil-blur".into())
            .spawn(move || run_worker(&theirs, make()))
            .ok();
        BlurEngine {
            shared,
            worker,
            pipe: None,
            mask: vec![0; MASK_LEN],
            mask_seq: None,
            seq: 0,
        }
    }

    /// Blurs the NV12 picture (`width` x `height`, row pitch `stride`) in place at `level`.
    pub fn apply(
        &mut self,
        data: &mut [u8],
        width: usize,
        height: usize,
        stride: usize,
        level: Blur,
    ) -> BlurStatus {
        let strength = match level {
            Blur::Off => return BlurStatus::Blurred,
            Blur::Light => Strength {
                radius: 2,
                passes: 3,
            },
            Blur::Strong => Strength {
                radius: 4,
                passes: 3,
            },
        };
        if !fits(data.len(), width, height, stride) {
            return BlurStatus::Blurred;
        }
        if self
            .pipe
            .as_ref()
            .is_none_or(|p| p.w != width || p.h != height)
        {
            self.pipe = Some(Pipeline::new(width, height));
        }
        let Some(pipe) = self.pipe.as_mut() else {
            return BlurStatus::Blurred;
        };
        self.seq += 1;
        pipe.downscale(data, stride);
        pipe.make_input();
        {
            let mut slot = lock(&self.shared.input);
            std::mem::swap(&mut slot.buf, &mut pipe.input);
            slot.seq = self.seq;
            slot.ready = true;
        }
        self.shared.wake.notify_one();
        let deadline = Instant::now() + MASK_WAIT;
        take_mask(
            &self.shared,
            &mut self.mask,
            &mut self.mask_seq,
            self.seq,
            deadline,
        );
        if let Some(seen) = self.mask_seq
            && self.seq - seen > MAX_MASK_AGE
        {
            return BlurStatus::TooBusy;
        }
        pipe.blur(strength, &self.mask);
        pipe.composite(data, stride, &self.mask);
        BlurStatus::Blurred
    }
}

impl BlurEngine {
    /// Pictures between the newest picture and the mask it was blended with (tests).
    #[cfg(test)]
    fn mask_age(&self) -> Option<u64> {
        self.mask_seq.map(|m| self.seq - m)
    }
}

impl Drop for BlurEngine {
    fn drop(&mut self) {
        lock(&self.shared.input).stop = true;
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Takes the newest mask into `mask`, waiting until `deadline` for the one of picture `seq`.
fn take_mask(
    shared: &Shared,
    mask: &mut Vec<u8>,
    mask_seq: &mut Option<u64>,
    seq: u64,
    deadline: Instant,
) {
    let mut out = lock(&shared.out);
    while out.seq < seq {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        out = match shared.done.wait_timeout(out, deadline - now) {
            Ok((guard, _)) => guard,
            Err(poisoned) => poisoned.into_inner().0,
        };
    }
    if out.fresh {
        std::mem::swap(mask, &mut out.buf);
        *mask_seq = Some(out.seq);
        out.fresh = false;
    }
}

/// Takes the newest input, segments it, and leaves the mask in the "out" slot, until stopped.
fn run_worker(shared: &Shared, mut segment: impl FnMut(&[f32], &mut [u8])) {
    let mut input = vec![0.0f32; INPUT_LEN];
    let mut mask = vec![0u8; MASK_LEN];
    loop {
        let seq = {
            let mut slot = lock(&shared.input);
            while !slot.ready && !slot.stop {
                slot = shared
                    .wake
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if slot.stop {
                return;
            }
            std::mem::swap(&mut input, &mut slot.buf);
            slot.ready = false;
            slot.seq
        };
        segment(&input, &mut mask);
        {
            let mut out = lock(&shared.out);
            std::mem::swap(&mut mask, &mut out.buf);
            out.seq = seq;
            out.fresh = true;
        }
        shared.done.notify_all();
    }
}

/// True when the buffer holds a whole even-sized NV12 picture at this pitch.
fn fits(len: usize, width: usize, height: usize, stride: usize) -> bool {
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || stride < width
    {
        return false;
    }
    stride
        .checked_mul(height + height / 2 - 1)
        .and_then(|n| n.checked_add(width))
        .is_some_and(|needed| len >= needed)
}
