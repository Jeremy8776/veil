//! A bandwidth cap per relay session, so one member cannot use the relay without limit.
//!
//! Purpose: each member's link keeps one [`Bucket`] per relay session it sends on, shared by
//! the datagram and stream paths. A packet that finds too few tokens is dropped, as a
//! congested network would drop it; the end-to-end QUIC link inside the session then slows
//! down by itself.
//!
//! Invariants:
//! - A session may send [`RATE`] bytes per second on average and [`BURST`] at once.
//! - The bucket is refilled from elapsed time only, so no timer runs per session.
//!
//! The server also keeps one [`FairShare`] for all relayed bytes ([`TOTAL_RATE`] by default), so
//! the sum of every session cannot exceed what its operator offered, and a sender that floods
//! the relay cannot starve the others.
//!
//! Fair sharing invariants:
//! - Senders are counted by address block, not by key or session: keys and sessions are free
//!   to make, addresses are not.
//! - While more than half of the total burst is left, any sender may use it, so spare capacity
//!   is never wasted.
//! - Below half, a sender gets only its equal part of [`SHARE_WINDOW`]. That keeps the rest of
//!   the burst for senders still under their part.
//! - Packets over the limit are dropped, never queued, so the relay holds no extra memory.
//!
//! shortcut: people behind one carrier-grade NAT address count as one sender, so they split one
//! part when the relay is full. Weigh a block by its sessions if busy mobile networks suffer.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// How often each sender's fair part starts again.
pub const SHARE_WINDOW: Duration = Duration::from_millis(100);

/// Longer than it takes to refill from empty, so dropping an idle bucket changes nothing.
pub const IDLE: Duration = Duration::from_secs(30);

/// 16 Mbit/s: a 1080p stream with its thumbnail and sound, with headroom.
pub const RATE: f64 = 2_000_000.0;
/// One keyframe's worth at once.
pub const BURST: f64 = 1_000_000.0;
/// Every relay session on one server together: 1 Gbit/s.
pub const TOTAL_RATE: f64 = 125_000_000.0;

pub struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}

impl Bucket {
    /// One session's bucket: [`RATE`] and [`BURST`].
    pub fn new(now: Instant) -> Self {
        Self::with_rate(RATE, BURST, now)
    }

    /// A full bucket that refills at `rate` bytes per second, holding at most `burst`.
    pub fn with_rate(rate: f64, burst: f64, now: Instant) -> Self {
        Self {
            tokens: burst,
            last: now,
            rate,
            burst,
        }
    }

    /// Unused for [`IDLE`]: its session has most likely ended, and a fresh bucket would be
    /// full anyway, so it can be dropped.
    pub fn idle(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last) > IDLE
    }

    /// Takes `bytes` if there is room for them now.
    pub fn take(&mut self, bytes: usize, now: Instant) -> bool {
        self.refill(now);
        // Exact for any packet a relay carries (at most 64 KiB).
        let cost = bytes as f64;
        if cost > self.tokens {
            return false;
        }
        self.tokens -= cost;
        true
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
    }
}

/// The relay's total budget, shared equally between sending address blocks when it runs short.
pub struct FairShare {
    total: Bucket,
    window_start: Instant,
    /// Bytes each recent sender was given in this window. Only senders in a relay session reach
    /// this, and senders silent for a whole window are dropped, so it stays bounded.
    senders: HashMap<IpAddr, f64>,
}

impl FairShare {
    /// A full budget of `rate` bytes per second, holding at most a tenth of a second.
    pub fn new(rate: f64, now: Instant) -> Self {
        Self {
            total: Bucket::with_rate(rate, rate / 10.0, now),
            window_start: now,
            senders: HashMap::new(),
        }
    }

    /// Takes `bytes` for `sender` (an address block) if the budget and its fair part allow it.
    pub fn take(&mut self, sender: IpAddr, bytes: usize, now: Instant) -> bool {
        self.start_window_if_due(now);
        self.total.refill(now);
        let used = *self.senders.entry(sender).or_insert(0.0);
        let cost = bytes as f64;
        let part = self.total.rate * SHARE_WINDOW.as_secs_f64() / self.senders.len() as f64;
        let short = self.total.tokens < self.total.burst / 2.0;
        // A sender's first packet of a window always qualifies, so a part smaller than one
        // packet cannot shut it out.
        if short && used > 0.0 && used + cost > part {
            return false;
        }
        if !self.total.take(bytes, now) {
            return false;
        }
        self.senders.insert(sender, used + cost);
        true
    }

    fn start_window_if_due(&mut self, now: Instant) {
        if now.saturating_duration_since(self.window_start) < SHARE_WINDOW {
            return;
        }
        self.window_start = now;
        self.senders.retain(|_, used| {
            let sent = *used > 0.0;
            *used = 0.0;
            sent
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 1_000_000.0;
    const PACKET: usize = 1_000;
    const TICK: Duration = Duration::from_millis(10);

    fn sender(n: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, n])
    }

    /// One simulated second. Each tick, every sender in turn offers one packet per round, for
    /// as many rounds as it has in `offered`. Returns the bytes each was given.
    fn run(offered: &[usize]) -> Vec<usize> {
        let start = Instant::now();
        let mut fair = FairShare::new(RATE, start);
        let mut given = vec![0; offered.len()];
        for tick in 1..=100 {
            let now = start + TICK * tick;
            for round in 0..offered.iter().copied().max().unwrap_or(0) {
                for (i, rounds) in offered.iter().enumerate() {
                    let n = u8::try_from(i).unwrap_or(u8::MAX);
                    if round < *rounds && fair.take(sender(n), PACKET, now) {
                        given[i] += PACKET;
                    }
                }
            }
        }
        given
    }

    #[test]
    fn a_lone_sender_can_use_the_whole_budget() {
        let given = run(&[100]);
        assert!(given[0] >= 900_000, "got {}", given[0]);
    }

    #[test]
    fn a_flood_cannot_starve_a_light_sender_or_waste_the_rest() {
        let given = run(&[100, 1]);
        assert_eq!(given[1], 100 * PACKET, "every light packet passes");
        assert!(given[0] >= 700_000, "the flood keeps what is spare: {}", given[0]);
    }

    #[test]
    fn two_floods_split_the_budget_evenly() {
        let given = run(&[100, 100]);
        let (low, high) = (given[0].min(given[1]), given[0].max(given[1]));
        assert!(low * 10 >= high * 8, "uneven split: {given:?}");
    }

    #[test]
    fn many_senders_together_stay_within_the_budget() {
        let given: usize = run(&[20; 50]).iter().sum();
        let most = RATE + RATE / 10.0;
        assert!(given as f64 <= most, "gave {given}");
        assert!(given >= 800_000, "gave only {given}");
    }

    #[test]
    fn silent_senders_are_forgotten() {
        let start = Instant::now();
        let mut fair = FairShare::new(RATE, start);
        assert!(fair.take(sender(1), PACKET, start));
        assert!(fair.take(sender(2), PACKET, start));
        let next = start + SHARE_WINDOW;
        assert!(fair.take(sender(1), PACKET, next));
        assert!(fair.take(sender(1), PACKET, next + SHARE_WINDOW));
        assert_eq!(fair.senders.len(), 1);
    }

    #[test]
    fn a_burst_passes_then_the_rate_holds() {
        let start = Instant::now();
        let mut bucket = Bucket::new(start);
        let packet = 1_000;
        let burst = (0..2_000).take_while(|_| bucket.take(packet, start)).count();
        assert_eq!(burst, 1_000, "the burst is BURST bytes");
        assert!(!bucket.take(packet, start), "then nothing until time passes");

        // A tenth of a second refills a tenth of RATE.
        let later = start + Duration::from_millis(100);
        let refilled = (0..2_000).take_while(|_| bucket.take(packet, later)).count();
        assert_eq!(refilled, 200);

        // A long pause refills to BURST, no further.
        let much_later = later + Duration::from_secs(60);
        let capped = (0..5_000).take_while(|_| bucket.take(packet, much_later)).count();
        assert_eq!(capped, 1_000);
    }
}
