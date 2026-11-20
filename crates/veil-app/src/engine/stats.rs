//! Live call stats for the debug overlay: per link round trip, loss and bit rates, and per
//! person the received video size and frame rate.
//!
//! Purpose: the session task calls [`Sampler::sample`] once a second with each link's QUIC
//! counters (`MediaSession::link_stats`) and each person's count of decoded pictures, and
//! stores the result in `Shared` for the interface. Rates come from the change since the
//! last sample.
//!
//! Invariants:
//! - Nothing here is sent anywhere: the numbers stay on this device.
//! - A counter that went down (a new link after re-routing) restarts from that sample, so a
//!   rate is never negative or huge.
//! - The first sample of a link or person shows no rate yet (`None`), rather than a guess.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use veil_call::{LinkStats, Path};
use veil_security::PublicId;

use crate::model::LinkPath;

/// One link as the overlay shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRow {
    /// The person this link reaches; `None` for a forwarder (it carries everyone).
    pub id: Option<u32>,
    pub path: LinkPath,
    /// The link runs over IPv6.
    pub ipv6: bool,
    pub rtt: Duration,
    /// Packets lost in the last second, as a percentage of those sent.
    pub loss: Option<f32>,
    pub down_kbps: Option<u32>,
    pub up_kbps: Option<u32>,
}

/// One person's received video, as decoded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoRow {
    pub width: u16,
    pub height: u16,
    pub fps: Option<f32>,
}

/// Everything the overlay shows that the engine measures.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub links: Vec<LinkRow>,
    pub video: HashMap<u32, VideoRow>,
}

/// Decoded video from one person since the call began: size of the newest picture and
/// how many pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: u16,
    pub height: u16,
    pub frames: u64,
}

/// Kilobits a second for `bytes` over `elapsed`; `None` if the time is too short to tell.
pub fn kbps(bytes: u64, elapsed: Duration) -> Option<u32> {
    let secs = elapsed.as_secs_f64();
    if secs < 0.05 {
        return None;
    }
    let kbps = bytes as f64 * 8.0 / 1_000.0 / secs;
    Some(kbps.round().min(f64::from(u32::MAX)) as u32)
}

/// Lost as a percentage of sent; `None` when nothing was sent.
pub fn loss_percent(sent: u64, lost: u64) -> Option<f32> {
    (sent > 0).then(|| (lost.min(sent) as f64 * 100.0 / sent as f64) as f32)
}

/// Pictures a second over `elapsed`.
pub fn fps(frames: u64, elapsed: Duration) -> Option<f32> {
    let secs = elapsed.as_secs_f64();
    (secs >= 0.05).then(|| (frames as f64 / secs) as f32)
}

/// The last counters seen per link and per person, to turn totals into rates.
#[derive(Default)]
pub struct Sampler {
    links: HashMap<PublicId, (LinkStats, Instant)>,
    frames: HashMap<u32, (u64, Instant)>,
}

impl Sampler {
    /// Turns this second's counters into rows. `person` names the person a linked device is,
    /// if it is one; links to anything else (the forwarder) get `id: None`.
    pub fn sample(
        &mut self,
        links: Vec<(PublicId, LinkStats)>,
        decoded: &HashMap<u32, Decoded>,
        person: impl Fn(&PublicId) -> Option<u32>,
        now: Instant,
    ) -> Stats {
        let mut rows = Vec::with_capacity(links.len());
        let mut seen = HashMap::with_capacity(links.len());
        for (peer, link) in links {
            let mut row = LinkRow {
                id: person(&peer),
                path: link_path(&link.path),
                ipv6: link.ipv6,
                rtt: link.rtt,
                loss: None,
                down_kbps: None,
                up_kbps: None,
            };
            if let Some((before, at)) = self.links.get(&peer)
                && link.sent_packets >= before.sent_packets
                && link.lost_packets >= before.lost_packets
                && link.sent_bytes >= before.sent_bytes
                && link.received_bytes >= before.received_bytes
            {
                let elapsed = now.saturating_duration_since(*at);
                row.loss = loss_percent(
                    link.sent_packets - before.sent_packets,
                    link.lost_packets - before.lost_packets,
                );
                row.up_kbps = kbps(link.sent_bytes - before.sent_bytes, elapsed);
                row.down_kbps = kbps(link.received_bytes - before.received_bytes, elapsed);
            }
            seen.insert(peer, (link, now));
            rows.push(row);
        }
        self.links = seen;
        let mut video = HashMap::with_capacity(decoded.len());
        let mut frames = HashMap::with_capacity(decoded.len());
        for (id, d) in decoded {
            let rate = self
                .frames
                .get(id)
                .filter(|(before, _)| d.frames >= *before)
                .and_then(|(before, at)| {
                    fps(d.frames - before, now.saturating_duration_since(*at))
                });
            video.insert(
                *id,
                VideoRow {
                    width: d.width,
                    height: d.height,
                    fps: rate,
                },
            );
            frames.insert(*id, (d.frames, now));
        }
        self.frames = frames;
        rows.sort_by_key(|r| r.id.unwrap_or(u32::MAX));
        Stats { links: rows, video }
    }
}

/// The interface's name for how a link reaches someone.
pub(super) fn link_path(path: &Path) -> LinkPath {
    match path {
        Path::Direct => LinkPath::Direct,
        Path::Punched => LinkPath::Punched,
        Path::Relayed => LinkPath::Relayed,
        Path::Forwarder => LinkPath::Forwarder,
    }
}

#[cfg(test)]
mod tests {
    use veil_call::Path;

    use super::*;

    fn link(sent: u64, lost: u64, up: u64, down: u64) -> LinkStats {
        LinkStats {
            path: Path::Direct,
            ipv6: false,
            rtt: Duration::from_millis(23),
            sent_packets: sent,
            lost_packets: lost,
            sent_bytes: up,
            received_bytes: down,
        }
    }

    #[test]
    fn rates_need_time_to_pass() {
        assert_eq!(kbps(125_000, Duration::from_secs(1)), Some(1_000));
        assert_eq!(kbps(125_000, Duration::from_millis(500)), Some(2_000));
        assert_eq!(kbps(1, Duration::ZERO), None);
        assert_eq!(fps(30, Duration::from_secs(1)), Some(30.0));
        assert_eq!(fps(30, Duration::from_millis(10)), None);
    }

    #[test]
    fn loss_is_a_share_of_what_was_sent() {
        assert_eq!(loss_percent(0, 0), None);
        assert_eq!(loss_percent(200, 1), Some(0.5));
        assert_eq!(loss_percent(10, 20), Some(100.0), "never above everything");
    }

    #[test]
    fn samples_turn_totals_into_rates() {
        let peer = PublicId([7; 32]);
        let forwarder = PublicId([9; 32]);
        let who = |d: &PublicId| (*d == peer).then_some(4);
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        let mut sampler = Sampler::default();
        let mut decoded = HashMap::from([(
            4,
            Decoded {
                width: 1280,
                height: 720,
                frames: 100,
            },
        )]);

        let first = sampler.sample(
            vec![
                (peer, link(100, 0, 10_000, 20_000)),
                (forwarder, link(0, 0, 0, 0)),
            ],
            &decoded,
            who,
            t0,
        );
        assert_eq!(first.links.len(), 2);
        assert_eq!(first.links[0].id, Some(4), "people first, forwarder last");
        assert_eq!(first.links[1].id, None);
        assert_eq!(first.links[0].up_kbps, None, "no rate from one sample");
        assert_eq!(first.video[&4].fps, None);

        if let Some(d) = decoded.get_mut(&4) {
            d.frames = 130;
        }
        let second = sampler.sample(
            vec![(peer, link(300, 2, 135_000, 270_000))],
            &decoded,
            who,
            t1,
        );
        let row = &second.links[0];
        assert_eq!(row.up_kbps, Some(1_000));
        assert_eq!(row.down_kbps, Some(2_000));
        assert_eq!(row.loss, Some(1.0));
        assert_eq!(row.rtt, Duration::from_millis(23));
        assert_eq!(second.video[&4].fps, Some(30.0));
        assert_eq!(second.video[&4].width, 1280);
    }

    #[test]
    fn a_new_link_starts_again() {
        let peer = PublicId([7; 32]);
        let t0 = Instant::now();
        let mut sampler = Sampler::default();
        let none = HashMap::new();
        sampler.sample(
            vec![(peer, link(500, 5, 500_000, 500_000))],
            &none,
            |_| None,
            t0,
        );
        let after = sampler.sample(
            vec![(peer, link(10, 0, 1_000, 1_000))],
            &none,
            |_| None,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(after.links[0].up_kbps, None);
        assert_eq!(after.links[0].loss, None);
    }
}
