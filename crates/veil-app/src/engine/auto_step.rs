//! Auto data use: decides, once a second, whether a slow link should make Auto save data.
//!
//! Purpose: with Data use on Auto, Veil sends the full camera ladder while the links keep up,
//! and drops to the Data saver rungs (one small layer, lower-rate sound, the smallest layer
//! asked of a forwarder) when they do not. It steps back up once they recover. The session
//! task feeds [`AutoStep::update`] the same per-link rows the stats overlay shows
//! (`stats.rs`: round trip, loss, bit rate down and up), so there is no extra probe.
//!
//! How a second is judged, per link:
//! - Bad: loss of [`LOSS_BAD`] or more, or a round trip [`RTT_BAD`] above the link's own
//!   baseline (its lowest round trip lately), or a bit rate that fell to under half its
//!   recent average while loss or delay also rose.
//! - Good: loss under [`LOSS_GOOD`] and a round trip within [`RTT_GOOD`] of the baseline.
//! - Anything between is neither.
//!
//! The worst link decides, since one encoder feeds every link.
//!
//! Invariants:
//! - Hysteresis: stepping down needs [`BAD_TO_FALL`] bad seconds out of the last
//!   [`WINDOW`]; stepping up needs a run of good seconds with no break (the hold-down).
//! - The hold-down starts at [`HOLD_MIN`] seconds and doubles, up to [`HOLD_MAX`], each time
//!   the link fails again within [`RELAPSE`] seconds of a step up, so it cannot flap. A long
//!   spell at full resets it.
//! - A link's baseline restarts when its path changes (a new route has its own delay).
//! - A second with no measured loss yet (a new link) is not judged.
//!
//! shortcut: two steps only, full and Data saver; Auto never turns the camera off. Add a
//! step to Audio only if field tests show Data saver still does not fit on 2G-class links.
//! shortcut: stepping up is a guess after a quiet spell, not a measured probe of spare
//! capacity. If it overshoots in the 5G tests, add padding probes before the step up.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use super::stats::LinkRow;
use crate::model::LinkPath;

/// Loss, in percent of packets sent, that makes a second bad.
pub const LOSS_BAD: f32 = 5.0;
/// Loss under which a second can be good.
pub const LOSS_GOOD: f32 = 1.0;
/// Round trip above the baseline that makes a second bad.
pub const RTT_BAD: Duration = Duration::from_millis(150);
/// Round trip above the baseline under which a second can be good.
pub const RTT_GOOD: Duration = Duration::from_millis(50);
/// Delay above the baseline that, with a bit rate collapse, makes a second bad.
const RTT_RISING: Duration = Duration::from_millis(60);
/// Seconds looked back over when deciding to step down.
pub const WINDOW: usize = 5;
/// Bad seconds out of [`WINDOW`] that step down.
pub const BAD_TO_FALL: usize = 3;
/// Shortest run of good seconds before stepping up.
pub const HOLD_MIN: u32 = 15;
/// Longest hold-down.
pub const HOLD_MAX: u32 = 120;
/// A step down this many seconds or fewer after a step up doubles the next hold-down.
pub const RELAPSE: u32 = 60;
/// Seconds at full after which the hold-down goes back to [`HOLD_MIN`].
const SETTLED: u32 = 300;
/// Seconds of round trip kept per link to find its baseline.
const BASELINE_SECS: usize = 60;
/// Seconds of bit rate kept per link for its recent average.
const RATE_SECS: usize = 5;
/// A recent average under this, in kbit/s, is too small to call a fall a collapse.
const RATE_FLOOR: u32 = 300;

/// How one second looked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Good,
    Neither,
    Bad,
}

/// What one link looked like lately.
struct LinkState {
    path: LinkPath,
    rtts: VecDeque<Duration>,
    rates: VecDeque<u32>,
}

impl LinkState {
    fn new(path: LinkPath) -> Self {
        Self {
            path,
            rtts: VecDeque::with_capacity(BASELINE_SECS),
            rates: VecDeque::with_capacity(RATE_SECS),
        }
    }

    /// Judges this second against what came before, then remembers it.
    fn judge(&mut self, row: &LinkRow, loss: f32) -> Verdict {
        let baseline = self.rtts.iter().copied().min().unwrap_or(row.rtt).min(row.rtt);
        let excess = row.rtt.saturating_sub(baseline);
        let rate = row.down_kbps.unwrap_or(0).max(row.up_kbps.unwrap_or(0));
        let count = u32::try_from(self.rates.len()).unwrap_or(u32::MAX);
        let average = (count > 0).then(|| self.rates.iter().sum::<u32>() / count);
        let collapsed = average.is_some_and(|avg| avg >= RATE_FLOOR && rate < avg / 2);
        let strained = loss >= LOSS_GOOD || excess >= RTT_RISING;
        let verdict = if loss >= LOSS_BAD || excess >= RTT_BAD || (collapsed && strained) {
            Verdict::Bad
        } else if loss < LOSS_GOOD && excess < RTT_GOOD {
            Verdict::Good
        } else {
            Verdict::Neither
        };
        push(&mut self.rtts, row.rtt, BASELINE_SECS);
        push(&mut self.rates, rate, RATE_SECS);
        verdict
    }
}

fn push<T>(queue: &mut VecDeque<T>, value: T, cap: usize) {
    if queue.len() == cap {
        queue.pop_front();
    }
    queue.push_back(value);
}

/// Auto's decision and what it is based on. Call [`AutoStep::update`] once a second.
pub struct AutoStep {
    links: HashMap<Option<u32>, LinkState>,
    recent: VecDeque<Verdict>,
    saving: bool,
    /// Good seconds in a row while saving.
    good_run: u32,
    /// Good seconds needed to step up.
    hold: u32,
    /// Seconds since the last step up, while at full.
    since_rise: Option<u32>,
}

impl Default for AutoStep {
    fn default() -> Self {
        Self {
            links: HashMap::new(),
            recent: VecDeque::with_capacity(WINDOW),
            saving: false,
            good_run: 0,
            hold: HOLD_MIN,
            since_rise: None,
        }
    }
}

impl AutoStep {
    /// Takes this second's link rows and returns whether Auto should save data.
    pub fn update(&mut self, rows: &[LinkRow]) -> bool {
        self.links.retain(|id, _| rows.iter().any(|r| r.id == *id));
        let mut worst = None;
        for row in rows {
            let Some(loss) = row.loss else { continue };
            let state = self
                .links
                .entry(row.id)
                .or_insert_with(|| LinkState::new(row.path));
            if state.path != row.path {
                *state = LinkState::new(row.path);
            }
            let verdict = state.judge(row, loss);
            worst = worst.max(Some(verdict));
        }
        if let Some(verdict) = worst {
            self.step(verdict);
        }
        self.saving
    }

    fn step(&mut self, verdict: Verdict) {
        push(&mut self.recent, verdict, WINDOW);
        if self.saving {
            self.good_run = if verdict == Verdict::Good {
                self.good_run + 1
            } else {
                0
            };
            if self.good_run >= self.hold {
                self.saving = false;
                self.since_rise = Some(0);
                self.recent.clear();
            }
            return;
        }
        if let Some(since) = &mut self.since_rise {
            *since += 1;
            if *since >= SETTLED {
                self.hold = HOLD_MIN;
                self.since_rise = None;
            }
        }
        let bad = self.recent.iter().filter(|v| **v == Verdict::Bad).count();
        if bad >= BAD_TO_FALL {
            if self.since_rise.is_some_and(|s| s <= RELAPSE) {
                self.hold = (self.hold * 2).min(HOLD_MAX);
            }
            self.saving = true;
            self.good_run = 0;
            self.since_rise = None;
            self.recent.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rtt_ms: u64, loss: f32, kbps: u32) -> LinkRow {
        LinkRow {
            id: Some(1),
            path: LinkPath::Direct,
            ipv6: false,
            rtt: Duration::from_millis(rtt_ms),
            loss: Some(loss),
            down_kbps: Some(kbps),
            up_kbps: Some(kbps),
        }
    }

    /// Feeds `secs` identical seconds and returns the last decision.
    fn feed(auto: &mut AutoStep, secs: u32, r: &LinkRow) -> bool {
        (0..secs).fold(false, |_, _| auto.update(std::slice::from_ref(r)))
    }

    fn calm(auto: &mut AutoStep) {
        assert!(!feed(auto, 10, &row(40, 0.0, 2_000)));
    }

    #[test]
    fn a_healthy_link_stays_at_full() {
        let mut auto = AutoStep::default();
        assert!(!feed(&mut auto, 120, &row(40, 0.2, 2_000)));
    }

    #[test]
    fn rising_loss_steps_down() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(!feed(&mut auto, 2, &row(40, 8.0, 2_000)), "two bad seconds are not enough");
        assert!(feed(&mut auto, 1, &row(40, 8.0, 2_000)));
    }

    #[test]
    fn rising_round_trip_steps_down() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(feed(&mut auto, 3, &row(260, 0.0, 2_000)));
    }

    #[test]
    fn a_high_but_steady_round_trip_is_not_slow() {
        let mut auto = AutoStep::default();
        assert!(!feed(&mut auto, 60, &row(600, 0.0, 2_000)), "satellite delay is its baseline");
    }

    #[test]
    fn falling_bit_rate_with_delay_steps_down() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(feed(&mut auto, 3, &row(110, 0.0, 400)));
    }

    #[test]
    fn falling_bit_rate_alone_does_not() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(!feed(&mut auto, 20, &row(40, 0.0, 100)), "they may have turned video off");
    }

    #[test]
    fn single_spikes_do_not_flap() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        for _ in 0..30 {
            assert!(!auto.update(&[row(40, 9.0, 2_000)]));
            assert!(!feed(&mut auto, 2, &row(40, 0.0, 2_000)));
        }
    }

    #[test]
    fn steps_up_only_after_the_hold_down() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(feed(&mut auto, 3, &row(40, 10.0, 2_000)));
        assert!(feed(&mut auto, HOLD_MIN - 1, &row(40, 0.0, 200)));
        assert!(!feed(&mut auto, 1, &row(40, 0.0, 200)));
    }

    #[test]
    fn a_break_in_the_good_run_restarts_the_hold_down() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        assert!(feed(&mut auto, 3, &row(40, 10.0, 2_000)));
        assert!(feed(&mut auto, HOLD_MIN - 1, &row(40, 0.0, 200)));
        assert!(feed(&mut auto, 1, &row(40, 2.0, 200)), "middling loss is not good");
        assert!(feed(&mut auto, HOLD_MIN - 1, &row(40, 0.0, 200)));
        assert!(!feed(&mut auto, 1, &row(40, 0.0, 200)));
    }

    #[test]
    fn a_relapse_doubles_the_hold_down_and_settling_resets_it() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        let bad = row(40, 10.0, 2_000);
        let good = row(40, 0.0, 2_000);
        assert!(feed(&mut auto, 3, &bad));
        assert!(!feed(&mut auto, HOLD_MIN, &good));
        assert!(feed(&mut auto, 3, &bad), "fails again soon after");
        assert!(feed(&mut auto, HOLD_MIN, &good), "now waits longer");
        assert!(!feed(&mut auto, HOLD_MIN, &good));
        assert!(!feed(&mut auto, SETTLED, &good));
        assert!(feed(&mut auto, 3, &bad));
        assert!(!feed(&mut auto, HOLD_MIN, &good), "back to the short hold-down");
    }

    #[test]
    fn the_worst_link_decides_and_new_links_wait() {
        let mut auto = AutoStep::default();
        let fine = row(40, 0.0, 2_000);
        let mut poor = row(40, 10.0, 2_000);
        poor.id = Some(2);
        let mut fresh = poor.clone();
        fresh.loss = None;
        for _ in 0..10 {
            assert!(!auto.update(&[fine.clone(), fresh.clone()]));
        }
        for _ in 0..2 {
            assert!(!auto.update(&[fine.clone(), poor.clone()]));
        }
        assert!(auto.update(&[fine, poor]));
    }

    #[test]
    fn a_new_path_gets_a_new_baseline() {
        let mut auto = AutoStep::default();
        calm(&mut auto);
        let mut relayed = row(220, 0.0, 2_000);
        relayed.path = LinkPath::Relayed;
        assert!(!feed(&mut auto, 30, &relayed));
    }
}
