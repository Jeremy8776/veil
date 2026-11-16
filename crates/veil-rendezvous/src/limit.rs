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
//! The server also keeps one bucket for all relayed bytes ([`TOTAL_RATE`] by default), so the
//! sum of every session cannot exceed what its operator offered.
//!
//! shortcut: caps, not fair queuing. Sessions do not share spare capacity, and when the total
//! is spent the first packets to arrive win. Add fair queuing before running a relay for many
//! strangers.

use std::time::{Duration, Instant};

/// Longer than it takes to refill from empty, so dropping an idle bucket changes nothing.
pub const IDLE: Duration = Duration::from_secs(30);

/// 16 Mbit/s: a 1080p stream with its thumbnail and sound, with headroom.
pub const RATE: f64 = 2_000_000.0;
/// One keyframe's worth at once.
pub const BURST: f64 = 1_000_000.0;
/// Every relay session on one server together: 1 Gbit/s.
pub const TOTAL_RATE: f64 = 125_000_000.0;
/// A tenth of a second of [`TOTAL_RATE`].
pub const TOTAL_BURST: f64 = TOTAL_RATE / 10.0;

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
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        // Exact for any packet a relay carries (at most 64 KiB).
        let cost = bytes as f64;
        if cost > self.tokens {
            return false;
        }
        self.tokens -= cost;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
