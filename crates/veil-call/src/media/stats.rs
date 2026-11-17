//! Live numbers about each media link, for the app's call stats.
//!
//! Purpose: [`MediaSession::link_stats`] reads QUIC's own counters for every link (round trip,
//! packets sent and lost, bytes each way). Reading them sends nothing and changes nothing.
//!
//! Invariants:
//! - Counters only grow for the life of a link; a new link (after re-routing) starts again
//!   from zero, so a reader that keeps the last sample must allow for a drop.

use std::time::Duration;

use veil_security::PublicId;

use super::MediaSession;
use crate::node::Path;

/// One link's counters, as QUIC keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkStats {
    pub path: Path,
    /// QUIC's smoothed round-trip time.
    pub rtt: Duration,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}

impl MediaSession {
    /// Each linked peer (another member in a mesh, the forwarder otherwise) and its link's
    /// counters now.
    pub fn link_stats(&self) -> Vec<(PublicId, LinkStats)> {
        self.ctx.lock().map_or_else(Vec::new, |s| {
            s.links
                .iter()
                .map(|(peer, e)| {
                    let quic = e.carrier.link().quic();
                    let stats = quic.stats();
                    let link = LinkStats {
                        path: e.path,
                        rtt: quic.rtt(),
                        sent_packets: stats.path.sent_packets,
                        lost_packets: stats.path.lost_packets,
                        sent_bytes: stats.udp_tx.bytes,
                        received_bytes: stats.udp_rx.bytes,
                    };
                    (*peer, link)
                })
                .collect()
        })
    }
}
