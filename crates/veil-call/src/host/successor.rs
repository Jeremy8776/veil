//! Choosing who takes over a persistent call (ADR 0010 section 6).
//!
//! Purpose: turn members' coarse self-reports into the automatic successor order and the
//! Good/OK/Poor bar the leaving host sees. Pure functions, no I/O.
//!
//! Invariants:
//! - Only a member whose own report says it is available, who is not relay-only and who is
//!   not reachable only through a relay can be chosen.
//! - The order is total and stable: ties fall to the lower device key, so every run of the
//!   same reports gives the same successor.
//! - Coarsening happens on the reporting device, before the report leaves it.

use std::cmp::Reverse;
use std::time::Duration;

use veil_net::control::{Availability, Reachable};
use veil_security::PublicId;

/// The most successors a leaving host tries before it gives up.
pub const MAX_ATTEMPTS: usize = 3;

/// One member as the host sees it when choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub device: PublicId,
    pub report: Availability,
    /// The host's own knowledge: this member never sent direct addresses (Hide my address).
    pub relay_only: bool,
}

impl Candidate {
    fn eligible(&self) -> bool {
        self.report.available && !self.relay_only && self.report.reachable != Reachable::No
    }
}

/// What the leaving host sees beside each available person's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Good,
    Ok,
    Poor,
}

/// Eligible members, best first. The first is the automatic pick.
pub fn rank(candidates: &[Candidate]) -> Vec<PublicId> {
    let mut eligible: Vec<&Candidate> = candidates.iter().filter(|c| c.eligible()).collect();
    eligible.sort_by_key(|c| {
        (
            c.report.reachable,
            Reverse(c.report.direct_links),
            c.report.rtt_10ms,
            Reverse(c.report.upload_500k),
            c.device.0,
        )
    });
    eligible.iter().map(|c| c.device).collect()
}

/// The bar for an eligible member; `None` for one that cannot be chosen.
pub fn bar(candidate: &Candidate) -> Option<Bar> {
    if !candidate.eligible() {
        return None;
    }
    let report = candidate.report;
    Some(match (report.reachable, report.rtt_10ms) {
        (_, _) if report.direct_links == 0 => Bar::Poor,
        (Reachable::Good, rtt) if rtt <= 10 => Bar::Good,
        (_, rtt) if rtt > 25 => Bar::Poor,
        _ => Bar::Ok,
    })
}

/// Round trip in 10 ms steps, capped at 1 second.
pub fn coarse_rtt(rtt: Duration) -> u8 {
    u8::try_from((rtt.as_millis() / 10).min(100)).unwrap_or(100)
}

/// Upload in 500 kbit/s steps.
pub fn coarse_upload(kbps: u64) -> u16 {
    u16::try_from(kbps / 500).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(reachable: Reachable, links: u8, rtt: u8, up: u16) -> Availability {
        Availability {
            available: true,
            reachable,
            direct_links: links,
            rtt_10ms: rtt,
            upload_500k: up,
        }
    }

    fn candidate(byte: u8, report: Availability) -> Candidate {
        Candidate {
            device: PublicId([byte; 32]),
            report,
            relay_only: false,
        }
    }

    #[test]
    fn ranks_reachability_then_links_then_rtt_then_upload() {
        let list = [
            candidate(1, report(Reachable::Maybe, 9, 1, 99)),
            candidate(2, report(Reachable::Good, 2, 5, 10)),
            candidate(3, report(Reachable::Good, 3, 9, 10)),
            candidate(4, report(Reachable::Good, 3, 4, 10)),
            candidate(5, report(Reachable::Good, 3, 4, 20)),
        ];
        let order: Vec<u8> = rank(&list).iter().map(|d| d.0[0]).collect();
        assert_eq!(order, [5, 4, 3, 2, 1]);
    }

    #[test]
    fn ties_fall_to_the_lower_key() {
        let same = report(Reachable::Good, 2, 3, 4);
        let order: Vec<u8> = rank(&[candidate(9, same), candidate(3, same)])
            .iter()
            .map(|d| d.0[0])
            .collect();
        assert_eq!(order, [3, 9]);
    }

    #[test]
    fn unavailable_relay_only_and_relay_reached_members_are_never_chosen() {
        let mut off = candidate(1, report(Reachable::Good, 5, 1, 9));
        off.report.available = false;
        let mut hidden = candidate(2, report(Reachable::Good, 5, 1, 9));
        hidden.relay_only = true;
        let relayed = candidate(3, report(Reachable::No, 5, 1, 9));
        assert!(rank(&[off, hidden, relayed]).is_empty());
        assert_eq!(bar(&off), None);
        assert_eq!(bar(&hidden), None);
        assert_eq!(bar(&relayed), None);
    }

    #[test]
    fn bars() {
        assert_eq!(bar(&candidate(1, report(Reachable::Good, 2, 8, 1))), Some(Bar::Good));
        assert_eq!(bar(&candidate(1, report(Reachable::Maybe, 2, 8, 1))), Some(Bar::Ok));
        assert_eq!(bar(&candidate(1, report(Reachable::Good, 2, 20, 1))), Some(Bar::Ok));
        assert_eq!(bar(&candidate(1, report(Reachable::Good, 2, 30, 1))), Some(Bar::Poor));
        assert_eq!(bar(&candidate(1, report(Reachable::Good, 0, 1, 1))), Some(Bar::Poor));
    }

    #[test]
    fn coarsening() {
        assert_eq!(coarse_rtt(Duration::from_millis(37)), 3);
        assert_eq!(coarse_rtt(Duration::from_secs(9)), 100);
        assert_eq!(coarse_upload(2_400), 4);
        assert_eq!(coarse_upload(u64::MAX), u16::MAX);
    }
}
