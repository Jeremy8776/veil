//! A member's coarse availability report for a persistent call's host (ADR 0010 §6).
//!
//! Purpose: turn this device's own link rows into the few coarse numbers the host ranks
//! successors by. Pure, so it is tested on its own.
//!
//! Invariants:
//! - No address, no member list and no per-peer figure leaves the device: only counts, a
//!   median and a total, already coarsened.
//! - Hide my address always reports not available and not reachable.

use std::time::Duration;

use veil_call::host::successor::{coarse_rtt, coarse_upload};
use veil_net::control::{Availability, Reachable};

use super::stats::LinkRow;
use crate::model::LinkPath;

/// How often an available member reports, at most.
pub const REPORT_EVERY: Duration = Duration::from_secs(10);

/// What this device knows about how others reach it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reach {
    pub relay_only: bool,
    /// A public address: a global IPv6 address, an address set by hand, or a router port.
    pub public: bool,
}

pub fn report(available: bool, reach: Reach, links: &[LinkRow]) -> Availability {
    let direct: Vec<&LinkRow> = links
        .iter()
        .filter(|l| l.id.is_some() && matches!(l.path, LinkPath::Direct | LinkPath::Punched))
        .collect();
    let mut rtts: Vec<Duration> = direct.iter().map(|l| l.rtt).collect();
    rtts.sort();
    let median = rtts.get(rtts.len() / 2).copied().unwrap_or(Duration::from_secs(1));
    let upload: u64 = links.iter().filter_map(|l| l.up_kbps).map(u64::from).sum();
    let reachable = match reach {
        Reach {
            relay_only: true, ..
        } => Reachable::No,
        Reach { public: true, .. } => Reachable::Good,
        _ => Reachable::Maybe,
    };
    Availability {
        available: available && !reach.relay_only,
        reachable,
        direct_links: u8::try_from(direct.len()).unwrap_or(u8::MAX),
        rtt_10ms: coarse_rtt(median),
        upload_500k: coarse_upload(upload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: Option<u32>, path: LinkPath, rtt_ms: u64, up: u32) -> LinkRow {
        LinkRow {
            id,
            path,
            ipv6: false,
            rtt: Duration::from_millis(rtt_ms),
            loss: None,
            down_kbps: None,
            up_kbps: Some(up),
        }
    }

    #[test]
    fn counts_direct_links_and_coarsens_the_rest() {
        let links = [
            row(Some(1), LinkPath::Direct, 30, 1_000),
            row(Some(2), LinkPath::Punched, 50, 1_000),
            row(Some(3), LinkPath::Relayed, 5, 1_000),
            row(None, LinkPath::Forwarder, 5, 500),
        ];
        let r = report(true, Reach::default(), &links);
        assert!(r.available);
        assert_eq!(r.reachable, Reachable::Maybe);
        assert_eq!(r.direct_links, 2);
        assert_eq!(r.rtt_10ms, 5, "median of the direct links, 50 ms");
        assert_eq!(r.upload_500k, 7);
    }

    #[test]
    fn hide_my_address_is_never_available() {
        let reach = Reach {
            relay_only: true,
            public: true,
        };
        let r = report(true, reach, &[]);
        assert!(!r.available);
        assert_eq!(r.reachable, Reachable::No);
    }

    #[test]
    fn a_public_address_is_good_and_no_links_is_a_slow_median() {
        let reach = Reach {
            relay_only: false,
            public: true,
        };
        let r = report(true, reach, &[]);
        assert_eq!(r.reachable, Reachable::Good);
        assert_eq!(r.rtt_10ms, 100);
        assert_eq!(r.direct_links, 0);
    }
}
