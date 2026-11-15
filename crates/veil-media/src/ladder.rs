//! The bitrate ladder: each spatial layer's size, frame rate and bitrate.
//!
//! Purpose: one place that turns `docs/ARCHITECTURE.md` section 3.1 into encoder settings.
//! Every rung shares the capture clock; a rung's frame rate is set by the highest temporal
//! layer it is encoded at (see [`crate::temporal`]).
//!
//! Invariants, checked by [`Ladder::new`]:
//! - Rungs are in spatial order 0, 1, 2 with no gaps, and each is at least as large as the one
//!   below.
//! - Each rung's top temporal layer is below the stream's temporal level count.
//! - Width and height are even (4:2:0 chroma) and at most [`MAX_DIMENSION`].

use veil_security::layer::MAX_SPATIAL;

use crate::MediaError;
use crate::temporal::{MAX_LEVELS, rate};

/// Largest width or height any rung may have.
pub const MAX_DIMENSION: u16 = 8_192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Thumb,
    Hd,
    Uhd,
}

impl Tier {
    /// The spatial layer number this tier uses in the layer marker.
    pub fn spatial(self) -> u8 {
        match self {
            Self::Thumb => 0,
            Self::Hd => 1,
            Self::Uhd => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rung {
    pub tier: Tier,
    pub width: u16,
    pub height: u16,
    /// Highest temporal layer this rung is encoded at.
    pub top_temporal: u8,
    pub bitrate_bps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ladder {
    capture_fps: u32,
    levels: u8,
    rungs: Vec<Rung>,
}

impl Ladder {
    pub fn new(capture_fps: u32, levels: u8, rungs: Vec<Rung>) -> Result<Self, MediaError> {
        if capture_fps == 0 || capture_fps > 240 {
            return Err(MediaError::BadConfig("capture rate"));
        }
        if levels == 0 || levels > MAX_LEVELS {
            return Err(MediaError::BadConfig("temporal levels"));
        }
        if rungs.is_empty() || rungs.len() > usize::from(MAX_SPATIAL) + 1 {
            return Err(MediaError::BadConfig("rung count"));
        }
        let mut below: Option<&Rung> = None;
        for (index, rung) in rungs.iter().enumerate() {
            if usize::from(rung.tier.spatial()) != index {
                return Err(MediaError::BadConfig("rung order"));
            }
            if rung.top_temporal >= levels {
                return Err(MediaError::BadConfig("rung temporal layer"));
            }
            let even = rung.width % 2 == 0 && rung.height % 2 == 0;
            if !even || rung.width == 0 || rung.height == 0 {
                return Err(MediaError::BadConfig("rung size"));
            }
            if rung.width > MAX_DIMENSION || rung.height > MAX_DIMENSION {
                return Err(MediaError::BadConfig("rung size"));
            }
            if below.is_some_and(|b| rung.width < b.width || rung.height < b.height) {
                return Err(MediaError::BadConfig("rung size order"));
            }
            below = Some(rung);
        }
        Ok(Self {
            capture_fps,
            levels,
            rungs,
        })
    }

    /// Camera at 2160p60: Thumb 640x360 at 15 fps, Hd 1920x1080 at 30 fps, Uhd 3840x2160 at
    /// 60 fps, with the bitrates from the architecture's ladder.
    pub fn camera() -> Self {
        Self {
            capture_fps: 60,
            levels: 3,
            rungs: vec![
                rung(Tier::Thumb, 640, 360, 0, 500_000),
                rung(Tier::Hd, 1_920, 1_080, 1, 3_000_000),
                rung(Tier::Uhd, 3_840, 2_160, 2, 20_000_000),
            ],
        }
    }

    /// This ladder without the rungs above `top`, for a camera that cannot capture 4K or a
    /// link that cannot carry it.
    pub fn up_to(mut self, top: Tier) -> Self {
        self.rungs.retain(|r| r.tier <= top);
        self
    }

    pub fn capture_fps(&self) -> u32 {
        self.capture_fps
    }

    pub fn levels(&self) -> u8 {
        self.levels
    }

    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }

    /// The frame rate a rung is encoded at.
    pub fn fps(&self, rung: &Rung) -> u32 {
        rate(self.capture_fps, self.levels, rung.top_temporal)
    }
}

const fn rung(tier: Tier, width: u16, height: u16, top_temporal: u8, bitrate_bps: u32) -> Rung {
    Rung {
        tier,
        width,
        height,
        top_temporal,
        bitrate_bps,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn camera_ladder_matches_the_architecture() {
        let ladder = Ladder::camera();
        let fps: Vec<u32> = ladder.rungs().iter().map(|r| ladder.fps(r)).collect();
        assert_eq!(fps, [15, 30, 60]);
        let again = Ladder::new(60, 3, ladder.rungs().to_vec()).unwrap();
        assert_eq!(again, ladder);
    }

    #[test]
    fn up_to_drops_higher_rungs() {
        let ladder = Ladder::camera().up_to(Tier::Hd);
        assert_eq!(ladder.rungs().len(), 2);
        assert_eq!(ladder.rungs()[1].tier, Tier::Hd);
    }

    #[test]
    fn bad_ladders_are_refused() {
        let thumb = rung(Tier::Thumb, 640, 360, 0, 1);
        let hd = rung(Tier::Hd, 1_920, 1_080, 1, 1);
        assert!(Ladder::new(60, 3, vec![hd]).is_err(), "gap at spatial 0");
        assert!(Ladder::new(60, 3, vec![thumb, thumb]).is_err(), "repeated tier");
        assert!(Ladder::new(60, 1, vec![thumb, hd]).is_err(), "temporal above levels");
        assert!(Ladder::new(60, 3, vec![rung(Tier::Thumb, 641, 360, 0, 1)]).is_err());
        assert!(Ladder::new(60, 3, vec![thumb, rung(Tier::Hd, 320, 180, 1, 1)]).is_err());
        assert!(Ladder::new(0, 3, vec![thumb]).is_err());
        assert!(Ladder::new(60, 5, vec![thumb]).is_err());
        assert!(Ladder::new(60, 3, vec![]).is_err());
    }
}
