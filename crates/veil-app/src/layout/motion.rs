//! Retargetable tile motion: joins keep identities and never jump moving participants.
use eframe::egui::Rect;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy)]
struct Transition {
    from: Rect,
    to: Rect,
    alpha: f32,
    since: f64,
}
impl Transition {
    fn sample(self, now: f64, duration: f32) -> (Rect, f32, bool) {
        let t = if duration <= 0.0 {
            1.0
        } else {
            ((now - self.since) as f32 / duration).clamp(0.0, 1.0)
        };
        let ease = 1.0 - (1.0 - t).powi(3);
        (
            Rect::from_min_max(
                self.from.min.lerp(self.to.min, ease),
                self.from.max.lerp(self.to.max, ease),
            ),
            self.alpha + (1.0 - self.alpha) * ease,
            t < 1.0,
        )
    }
}
#[derive(Default)]
pub struct Motion {
    cells: HashMap<u64, Transition>,
}
pub struct Placement {
    pub key: u64,
    pub rect: Rect,
    pub opacity: f32,
}
pub struct Frame {
    pub cells: Vec<Placement>,
    pub moving: bool,
}
impl Motion {
    pub fn frame(&mut self, targets: &[(u64, Rect)], now: f64, duration: f32) -> Frame {
        let keys: HashSet<_> = targets.iter().map(|(key, _)| *key).collect();
        self.cells.retain(|key, _| keys.contains(key));
        let mut moving = false;
        let cells = targets
            .iter()
            .map(|(key, target)| {
                let transition = self.cells.entry(*key).or_insert(Transition {
                    from: Rect::from_center_size(target.center(), target.size() * 0.96),
                    to: *target,
                    alpha: 0.0,
                    since: now,
                });
                if (transition.to.min - target.min).length() > 0.1
                    || (transition.to.max - target.max).length() > 0.1
                {
                    let (from, alpha, _) = transition.sample(now, duration);
                    *transition = Transition {
                        from,
                        to: *target,
                        alpha,
                        since: now,
                    };
                }
                let (rect, opacity, active) = transition.sample(now, duration);
                moving |= active;
                Placement {
                    key: *key,
                    rect,
                    opacity,
                }
            })
            .collect();
        Frame { cells, moving }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};
    fn rect(x: f32) -> Rect {
        Rect::from_min_size(pos2(x, 0.0), vec2(100.0, 200.0))
    }
    #[test]
    fn joining_retargets_from_current_position_and_keeps_identity() {
        let mut motion = Motion::default();
        motion.frame(&[(7, rect(0.0))], 0.0, 0.24);
        motion.frame(&[(7, rect(0.0))], 1.0, 0.24);
        let start = motion.frame(&[(7, rect(150.0)), (9, rect(0.0))], 1.0, 0.24);
        assert_eq!(start.cells[0].key, 7);
        assert_eq!(start.cells[0].rect, rect(0.0));
        assert_eq!(start.cells[1].opacity, 0.0);
        let before = motion.frame(&[(7, rect(150.0)), (9, rect(0.0))], 1.1, 0.24);
        let retarget = motion.frame(
            &[(7, rect(300.0)), (9, rect(0.0)), (11, rect(450.0))],
            1.1,
            0.24,
        );
        assert_eq!(before.cells[0].rect, retarget.cells[0].rect);
        let end = motion.frame(&[(7, rect(300.0))], 2.0, 0.24);
        assert_eq!(end.cells[0].rect, rect(300.0));
        assert!(!end.moving);
        assert_eq!(motion.cells.len(), 1);
    }
    #[test]
    fn disabled_animations_snap_to_the_target_and_show_new_members() {
        let mut motion = Motion::default();
        let frame = motion.frame(&[(3, rect(20.0))], 0.0, 0.0);
        assert_eq!(frame.cells[0].rect, rect(20.0));
        assert_eq!(frame.cells[0].opacity, 1.0);
        assert!(!frame.moving);
    }
}
