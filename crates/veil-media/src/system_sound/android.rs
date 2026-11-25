//! Phone sound for an Android screen share, through `AudioPlaybackCapture` (SHARE-02).
//!
//! Purpose: [`PhoneSound`] captures what other apps play while the screen share's
//! MediaProjection runs, and turns it into frames of [`AudioSettings::share_sound`]. Java
//! (`ScreenShare.java`) owns the `AudioRecord`; Rust polls it without blocking.
//!
//! Invariants:
//! - It needs a running screen share and holds that share's projection, so the sound cannot
//!   outlive the picture.
//! - Veil's own call sound is left out: Java excludes Veil's own app, and call audio uses the
//!   voice-communication usage, which playback capture never matches.
//! - Apps that refuse capture are silent; nothing here can override that.
//! - Dropping it stops the capture and lets go of the projection.

use std::time::{Duration, Instant};

use super::{Assembler, Support};
use crate::MediaError;
use crate::audio::AudioSettings;
use crate::ffi::android_share as bridge;
use crate::projection::{Status, push_pcm16};
use crate::screen;

/// How often the reader asks Java for new sound while it waits for a frame.
const POLL: Duration = Duration::from_millis(5);

/// Whether phone sound can be shared: playback capture needs the microphone permission.
pub fn support() -> Support {
    if bridge::sound_permitted() {
        Support::Ready
    } else {
        Support::Unavailable("Needs microphone permission")
    }
}

/// What other apps on this phone play, during a screen share.
pub struct PhoneSound {
    generation: u32,
    frames: Assembler,
    samples: Vec<f32>,
}

impl PhoneSound {
    /// Starts capture for the running screen share.
    pub fn open() -> Result<Self, MediaError> {
        let generation =
            screen::hold_current().ok_or(MediaError::Unavailable("phone sound without a share"))?;
        if !bridge::sound_start(generation) {
            screen::let_go(generation);
            return Err(MediaError::Unavailable("phone sound capture"));
        }
        Ok(Self {
            generation,
            frames: Assembler::new(&AudioSettings::share_sound()),
            samples: Vec::new(),
        })
    }

    /// Waits for the next frame, at most until it is due.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        loop {
            let bytes = match bridge::sound_read(self.generation) {
                Ok(bytes) => bytes,
                // The whole share has ended: silence until the share thread notices, so
                // this is not reported as sound failing while the picture goes on.
                Err(_) if bridge::status(self.generation) != Status::Live => Vec::new(),
                Err(e) => return Err(e),
            };
            self.samples.clear();
            push_pcm16(&bytes, &mut self.samples);
            self.frames.push(&self.samples);
            let now = Instant::now();
            let deadline = self.frames.deadline(now);
            if self.frames.ready() || now >= deadline {
                break;
            }
            std::thread::sleep(POLL.min(deadline - now));
        }
        Ok(self.frames.take())
    }
}

impl Drop for PhoneSound {
    fn drop(&mut self) {
        bridge::sound_stop(self.generation);
        screen::let_go(self.generation);
    }
}
