//! The sender's plan for each captured picture: which layers to encode, and keyframes.
//!
//! Purpose: one decision per captured picture. [`Schedule::next`] numbers the picture, gives
//! it a temporal layer from the dyadic pattern, decides whether it is a keyframe, and lists
//! the layers to encode. Layers nobody wants are not encoded at all (layer pausing), which
//! saves upload and encoder load.
//!
//! Invariants:
//! - A keyframe is always temporal layer 0 and falls where the pattern restarts anyway (a
//!   multiple of its period). Hardware encoders running their own temporal pattern only
//!   honour a forced IDR there, so a keyframe may wait up to one period (4 pictures at 3
//!   levels, 67 ms at 60 fps).
//! - A layer's reference is the latest earlier picture of a lower temporal layer, or the
//!   previous layer-0 picture for layer 0. Lower layers never refer to higher ones.
//! - Keyframes come at most once per [`MIN_KEY_GAP`], however often receivers ask. A request
//!   inside the gap is kept and served when the gap ends.
//! - Lowering the wanted layers applies from the next picture. Raising them, or resuming
//!   after a pause, waits for a keyframe, because a layer that was not encoded has nothing to
//!   predict from. Until then the old layers are encoded.
//!
//! shortcut: the periodic keyframe ([`KEYFRAME_EVERY`]) is the fallback for lost requests.
//! Lengthen it once keyframe requests travel end to end (see `veil-forward` `subscribe.rs`).

use std::time::{Duration, Instant};

use veil_security::{Layer, Wanted};

use crate::ladder::Ladder;
use crate::temporal::{MAX_LEVELS, period, temporal_id};

/// Shortest time between two keyframes.
pub const MIN_KEY_GAP: Duration = Duration::from_millis(500);
/// Longest time between two keyframes.
pub const KEYFRAME_EVERY: Duration = Duration::from_secs(4);

/// What to do with one captured picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PicturePlan {
    pub picture: u64,
    pub temporal: u8,
    pub keyframe: bool,
    /// The picture every layer here is predicted from. `None` for a keyframe.
    pub reference: Option<u64>,
    /// One marker per spatial layer to encode, lowest first. Empty: skip this picture.
    pub layers: Vec<Layer>,
}

pub struct Schedule {
    ladder: Ladder,
    picture: u64,
    /// Pictures since the last keyframe; picks the temporal layer.
    phase: u64,
    /// The latest picture of each temporal layer since the last keyframe.
    latest: [Option<u64>; MAX_LEVELS as usize],
    last_key: Option<Instant>,
    key_due: bool,
    /// The highest layers any receiver wants. `None`: nobody is watching.
    wanted: Option<Wanted>,
    /// The layers encoded now: `wanted`, or lower while a raise waits for its keyframe.
    encoding: Option<Wanted>,
}

impl Schedule {
    /// Starts with every layer wanted. The first picture is a keyframe.
    pub fn new(ladder: Ladder) -> Self {
        Self {
            ladder,
            picture: 0,
            phase: 0,
            latest: [None; MAX_LEVELS as usize],
            last_key: None,
            key_due: true,
            wanted: Some(Wanted::ALL),
            encoding: None,
        }
    }

    pub fn ladder(&self) -> &Ladder {
        &self.ladder
    }

    /// A receiver lost the stream. The next picture at least [`MIN_KEY_GAP`] after the last
    /// keyframe will be one.
    pub fn request_keyframe(&mut self) {
        self.key_due = true;
    }

    /// Sets the highest layers anyone wants, for example from [`wanted_by`]. A lower setting
    /// applies from the next picture; a raise waits for the next keyframe.
    pub fn set_wanted(&mut self, wanted: Option<Wanted>) {
        let raised = match (self.encoding, wanted) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(old), Some(new)) => new.spatial > old.spatial || new.temporal > old.temporal,
        };
        self.key_due |= raised;
        self.wanted = wanted;
        self.encoding = match (self.encoding, wanted) {
            (Some(old), Some(new)) => Some(old.min(new)),
            _ => None,
        };
    }

    /// Plans the next captured picture.
    pub fn next(&mut self, now: Instant) -> PicturePlan {
        let picture = self.picture;
        self.picture += 1;
        if self.wanted.is_none() {
            self.phase += 1;
            return skip(picture, temporal_id(self.phase - 1, self.ladder.levels()));
        }

        let gap_ok = self
            .last_key
            .is_none_or(|at| now.duration_since(at) >= MIN_KEY_GAP);
        let stale = self
            .last_key
            .is_some_and(|at| now.duration_since(at) >= KEYFRAME_EVERY);
        let aligned = self.phase % period(self.ladder.levels()) == 0;
        let keyframe = ((self.key_due && gap_ok) || stale) && aligned;
        if keyframe {
            self.phase = 0;
            self.latest = [None; MAX_LEVELS as usize];
            self.encoding = self.wanted;
        }
        let temporal = temporal_id(self.phase, self.ladder.levels());
        self.phase += 1;

        let reference = if keyframe {
            None
        } else {
            self.reference(temporal)
        };
        let Some(wanted) = self.encoding.filter(|_| keyframe || reference.is_some()) else {
            // Resuming, and the keyframe is waiting for its gap.
            return skip(picture, temporal);
        };
        let layers: Vec<Layer> = self
            .ladder
            .rungs()
            .iter()
            .filter(|r| r.tier.spatial() <= wanted.spatial)
            .filter(|r| temporal <= r.top_temporal.min(wanted.temporal))
            .filter_map(|r| Layer::new(r.tier.spatial(), temporal, keyframe).ok())
            .collect();
        if layers.is_empty() {
            return skip(picture, temporal);
        }
        if keyframe {
            self.last_key = Some(now);
            self.key_due = false;
        }
        if let Some(slot) = self.latest.get_mut(usize::from(temporal)) {
            *slot = Some(picture);
        }
        PicturePlan {
            picture,
            temporal,
            keyframe,
            reference,
            layers,
        }
    }

    /// The latest encoded picture this temporal layer may be predicted from.
    fn reference(&self, temporal: u8) -> Option<u64> {
        let below = if temporal == 0 {
            1
        } else {
            usize::from(temporal)
        };
        self.latest.iter().take(below).flatten().copied().max()
    }
}

/// The highest layers any of these receivers wants. `None` if there are no receivers.
pub fn wanted_by(receivers: impl IntoIterator<Item = Wanted>) -> Option<Wanted> {
    receivers.into_iter().reduce(Wanted::max)
}

fn skip(picture: u64, temporal: u8) -> PicturePlan {
    PicturePlan {
        picture,
        temporal,
        keyframe: false,
        reference: None,
        layers: Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// The keyframe due at `at`: it must come within one pattern period, at its start.
    fn keyframe_by(s: &mut Schedule, at: Instant) -> PicturePlan {
        for _ in 0..period(s.ladder().levels()) {
            let plan = s.next(at);
            if plan.keyframe {
                assert_eq!(plan.temporal, 0);
                return plan;
            }
        }
        panic!("no keyframe within one period");
    }

    fn spatial(plan: &PicturePlan) -> Vec<u8> {
        plan.layers.iter().map(|l| l.spatial).collect()
    }

    #[test]
    fn camera_pattern_and_references() {
        let now = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        let plans: Vec<PicturePlan> = (0..5).map(|_| s.next(now)).collect();

        assert!(plans[0].keyframe && plans[0].reference.is_none());
        assert_eq!(spatial(&plans[0]), [0, 1, 2]);
        assert!(
            plans[0]
                .layers
                .iter()
                .all(|l| l.keyframe && l.temporal == 0)
        );
        // Phase 1: temporal 2, Uhd only, from picture 0.
        assert_eq!(
            (plans[1].temporal, plans[1].reference, spatial(&plans[1])),
            (2, Some(0), vec![2])
        );
        // Phase 2: temporal 1, Hd and Uhd, from picture 0.
        assert_eq!(
            (plans[2].temporal, plans[2].reference, spatial(&plans[2])),
            (1, Some(0), vec![1, 2])
        );
        // Phase 3: temporal 2, from the temporal 1 picture.
        assert_eq!((plans[3].temporal, plans[3].reference), (2, Some(2)));
        // Phase 4: temporal 0 again, every rung, from the last temporal 0 picture.
        assert_eq!(
            (plans[4].temporal, plans[4].reference, spatial(&plans[4])),
            (0, Some(0), vec![0, 1, 2])
        );
        assert!(!plans[4].keyframe);
    }

    #[test]
    fn requests_are_held_to_the_minimum_gap() {
        let start = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        assert!(s.next(start).keyframe);
        s.request_keyframe();
        assert!(!s.next(start + MIN_KEY_GAP / 2).keyframe, "too soon");
        let served = keyframe_by(&mut s, start + MIN_KEY_GAP);
        assert_eq!(served.picture % 4, 0, "on a pattern boundary");
        assert!(
            !s.next(start + MIN_KEY_GAP * 3).keyframe,
            "request served once"
        );
    }

    #[test]
    fn a_keyframe_comes_periodically() {
        let start = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        s.next(start);
        assert!(!s.next(start + KEYFRAME_EVERY / 2).keyframe);
        keyframe_by(&mut s, start + KEYFRAME_EVERY);
    }

    #[test]
    fn unwanted_layers_are_not_encoded() {
        let now = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        s.next(now);
        s.set_wanted(wanted_by([
            Wanted::THUMB,
            Wanted {
                spatial: 1,
                temporal: 1,
            },
        ]));
        let plans: Vec<PicturePlan> = (0..4).map(|_| s.next(now)).collect();
        assert!(plans[0].layers.is_empty(), "temporal 2 is not wanted");
        assert_eq!(spatial(&plans[1]), [1], "temporal 1: Hd only");
        assert_eq!(spatial(&plans[3]), [0, 1], "temporal 0: no Uhd");
        assert!(
            plans.iter().all(|p| !p.keyframe),
            "lowering needs no keyframe"
        );
    }

    #[test]
    fn raising_or_resuming_starts_with_a_keyframe() {
        let start = Instant::now();
        let later = start + MIN_KEY_GAP;
        let mut s = Schedule::new(Ladder::camera());
        s.set_wanted(Some(Wanted::THUMB));
        s.next(start);
        s.set_wanted(Some(Wanted::ALL));
        keyframe_by(&mut s, later);

        s.set_wanted(wanted_by([]));
        assert!(s.next(later).layers.is_empty(), "nobody watching");
        s.set_wanted(Some(Wanted::THUMB));
        let resumed = keyframe_by(&mut s, later + MIN_KEY_GAP);
        assert_eq!(resumed.layers.len(), 1);
    }

    #[test]
    fn a_raise_inside_the_gap_keeps_the_old_layers_until_the_keyframe() {
        let start = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        s.set_wanted(Some(Wanted::THUMB));
        assert!(s.next(start).keyframe);
        s.set_wanted(Some(Wanted::ALL));
        let early: Vec<PicturePlan> = (0..4).map(|_| s.next(start)).collect();
        assert!(early.iter().all(|p| !p.keyframe));
        assert!(early[..3].iter().all(|p| p.layers.is_empty()));
        assert_eq!(spatial(&early[3]), [0], "Thumb only, from the old keyframe");
        let key = keyframe_by(&mut s, start + MIN_KEY_GAP);
        assert_eq!(spatial(&key), [0, 1, 2]);
    }

    #[test]
    fn resuming_inside_the_gap_waits_for_the_keyframe() {
        let start = Instant::now();
        let mut s = Schedule::new(Ladder::camera());
        assert!(s.next(start).keyframe);
        s.set_wanted(None);
        s.set_wanted(Some(Wanted::ALL));
        assert!(s.next(start).layers.is_empty());
        keyframe_by(&mut s, start + MIN_KEY_GAP);
    }
}
