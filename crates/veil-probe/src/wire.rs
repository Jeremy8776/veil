//! The probe message and the reply statistics. Pure, so both are tested without a network.
//!
//! A probe travels sealed, as a video frame or an audio datagram, so a reply also proves the
//! two members share the epoch's keys. The pinger's own clock stamps a ping and the pong echoes
//! the stamp back, so a round trip never compares two machines' clocks.

use std::collections::BTreeMap;
use std::time::Duration;

/// Encoded length: kind, sequence, member, stamp.
pub const LEN: usize = 1 + 4 + 4 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ping,
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub kind: Kind,
    pub seq: u32,
    /// The pinger's leaf index. A pong carries the leaf it answers, so other members ignore it.
    pub pinger: u32,
    /// Microseconds on the pinger's own clock.
    pub stamp_us: u64,
}

impl Probe {
    pub fn encode(&self) -> [u8; LEN] {
        let mut out = [0; LEN];
        out[0] = match self.kind {
            Kind::Ping => 1,
            Kind::Pong => 2,
        };
        out[1..5].copy_from_slice(&self.seq.to_be_bytes());
        out[5..9].copy_from_slice(&self.pinger.to_be_bytes());
        out[9..].copy_from_slice(&self.stamp_us.to_be_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; LEN] = bytes.try_into().ok()?;
        let kind = match bytes[0] {
            1 => Kind::Ping,
            2 => Kind::Pong,
            _ => return None,
        };
        Some(Self {
            kind,
            seq: u32::from_be_bytes(bytes[1..5].try_into().ok()?),
            pinger: u32::from_be_bytes(bytes[5..9].try_into().ok()?),
            stamp_us: u64::from_be_bytes(bytes[9..].try_into().ok()?),
        })
    }

    /// The answer to this ping, carrying its sequence, pinger and stamp unchanged.
    pub fn pong(&self) -> Self {
        Self {
            kind: Kind::Pong,
            ..*self
        }
    }
}

/// Which way a probe travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    /// A reliable stream per frame, as video uses.
    Video,
    /// An unreliable datagram, as audio uses.
    Audio,
}

/// Replies per peer and channel since the last report.
#[derive(Debug, Default)]
pub struct Stats {
    sent: u32,
    replies: BTreeMap<(u32, Channel), Vec<Duration>>,
}

impl Stats {
    /// One ping went out on each channel.
    pub fn sent(&mut self) {
        self.sent = self.sent.saturating_add(1);
    }

    pub fn reply(&mut self, peer: u32, channel: Channel, rtt: Duration) {
        self.replies.entry((peer, channel)).or_default().push(rtt);
    }

    /// One line per peer and channel heard from, then starts a new window. A peer that never
    /// replied is not listed, since a ping goes to every link at once.
    pub fn report(&mut self) -> Vec<String> {
        let sent = std::mem::take(&mut self.sent);
        std::mem::take(&mut self.replies)
            .into_iter()
            .map(|((peer, channel), mut rtts)| {
                rtts.sort_unstable();
                let median = rtts.get(rtts.len() / 2).copied().unwrap_or_default();
                let worst = rtts.last().copied().unwrap_or_default();
                format!(
                    "member {peer} {channel:?}: {}/{sent} replies, median {:.1} ms, worst {:.1} ms",
                    rtts.len(),
                    ms(median),
                    ms(worst),
                )
            })
            .collect()
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_survives_encoding_and_its_pong_echoes_it() {
        let ping = Probe {
            kind: Kind::Ping,
            seq: 7,
            pinger: 3,
            stamp_us: 123_456_789,
        };
        assert_eq!(Probe::decode(&ping.encode()), Some(ping));
        let pong = ping.pong();
        assert_eq!(
            (pong.kind, pong.seq, pong.pinger, pong.stamp_us),
            (Kind::Pong, 7, 3, 123_456_789)
        );
    }

    #[test]
    fn wrong_length_or_kind_is_refused() {
        let bytes = Probe {
            kind: Kind::Ping,
            seq: 1,
            pinger: 0,
            stamp_us: 0,
        }
        .encode();
        assert_eq!(Probe::decode(&bytes[..LEN - 1]), None);
        let mut bad = bytes;
        bad[0] = 9;
        assert_eq!(Probe::decode(&bad), None);
    }

    #[test]
    fn report_gives_rate_median_and_worst_then_resets() {
        let mut stats = Stats::default();
        for _ in 0..4 {
            stats.sent();
        }
        for ms in [30, 10, 20] {
            stats.reply(1, Channel::Video, Duration::from_millis(ms));
        }
        stats.reply(1, Channel::Audio, Duration::from_millis(5));
        let lines = stats.report();
        assert_eq!(
            lines,
            [
                "member 1 Video: 3/4 replies, median 20.0 ms, worst 30.0 ms",
                "member 1 Audio: 1/4 replies, median 5.0 ms, worst 5.0 ms",
            ]
        );
        assert!(stats.report().is_empty());
    }
}
