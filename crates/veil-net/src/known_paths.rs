//! Bounded, node-local evidence of a recently authenticated transport endpoint.
//! A helper's assertion, unverified dial or another (including virtual) node cannot add it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use veil_security::PublicId;

const MAX_KNOWN: usize = 256;
const LIFETIME: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct KnownPaths(HashMap<PublicId, (SocketAddr, Instant)>);

fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => {
            SocketAddr::new(v6.ip().to_canonical(), v6.port())
        }
        other => other,
    }
}

impl KnownPaths {
    pub(crate) fn record(&mut self, peer: PublicId, addr: SocketAddr, now: Instant) {
        self.0
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < LIFETIME);
        if self.0.len() >= MAX_KNOWN
            && !self.0.contains_key(&peer)
            && let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| *key)
        {
            self.0.remove(&oldest);
        }
        self.0.insert(peer, (canonical(addr), now));
    }

    pub(crate) fn contains(&self, peer: PublicId, addr: SocketAddr, now: Instant) -> bool {
        self.0.get(&peer).is_some_and(|(known, seen)| {
            *known == canonical(addr) && now.saturating_duration_since(*seen) < LIFETIME
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn evidence_is_exact_scoped_expiring_and_bounded() {
        let now = Instant::now();
        let mut paths = KnownPaths::default();
        let peer = PublicId([1; 32]);
        let addr = "127.0.0.1:7000".parse().unwrap();
        paths.record(peer, addr, now);
        assert!(paths.contains(peer, addr, now));
        assert!(paths.contains(peer, "[::ffff:127.0.0.1]:7000".parse().unwrap(), now));
        assert!(!paths.contains(PublicId([2; 32]), addr, now));
        assert!(!paths.contains(peer, "127.0.0.1:7001".parse().unwrap(), now));
        assert!(!paths.contains(peer, addr, now + LIFETIME));
        for n in 0..=MAX_KNOWN {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&(n as u64).to_le_bytes());
            paths.record(PublicId(key), addr, now + Duration::from_millis(n as u64));
        }
        assert_eq!(paths.0.len(), MAX_KNOWN);
        assert!(!paths.contains(peer, addr, now));
    }
}
