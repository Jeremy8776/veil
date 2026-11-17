//! The host's router port for one call: opened when asked for, renewed, removed at the end.
//!
//! Purpose: wraps `veil_portmap::Lease` for `host.rs`. Opening takes up to a few seconds, so
//! the call starts at once with its LAN addresses and [`Router::next`] reports the public
//! address when the router gives one; the host then puts it first in a fresh invite. If the
//! router does not open the port, [`guide`] says what the person can do by hand.
//!
//! Invariants:
//! - Nothing is asked of the router unless the person turned it on (Settings > Connection, or
//!   the one-time question before their first hosted call).
//! - [`Router::next`] is cancel-safe for `tokio::select!`: a renewal interrupted half way is
//!   simply tried again, because the lease changes only once the router has answered.
//! - [`Router::close`] is bounded by `veil_portmap::CLOSE_LIMIT`, inside the engine's own
//!   close wait.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tokio::task::JoinHandle;
use veil_portmap::{Failure, Lease};

/// What changed with the router port.
pub enum Event {
    /// Guests can dial this address (new, or changed on renewal).
    Open(SocketAddr),
    /// No port was opened; the text is the guide.
    Failed(String),
}

pub struct Router {
    opening: Option<JoinHandle<Result<Lease, Failure>>>,
    lease: Option<Lease>,
    /// A failure known before asking the router, reported by the first `next`.
    failed: Option<String>,
    port: u16,
    lan: Option<Ipv4Addr>,
}

impl Router {
    /// Starts opening `port` if `enabled` and this machine has an IPv4 LAN address.
    /// Returns `None` when there is nothing to do.
    pub fn start(enabled: bool, lan: Option<IpAddr>, port: u16) -> Option<Self> {
        if !enabled {
            return None;
        }
        let lan = match lan {
            Some(IpAddr::V4(ip)) => Some(ip),
            _ => None,
        };
        let opening = lan.map(|ip| tokio::spawn(Lease::open(ip, port)));
        let failed = lan
            .is_none()
            .then(|| guide(&Failure::Unavailable(Vec::new()), port, None));
        Some(Self {
            opening,
            lease: None,
            failed,
            port,
            lan,
        })
    }

    /// Waits for the next change. Pending for ever once there is nothing left to wait for.
    pub async fn next(&mut self) -> Event {
        if let Some(text) = self.failed.take() {
            return Event::Failed(text);
        }
        if let Some(opening) = &mut self.opening {
            let result = opening.await;
            self.opening = None;
            return match result {
                Ok(Ok(lease)) => {
                    crate::note!("router: {} opened {}", lease.protocol(), lease.external());
                    let addr = SocketAddr::V4(lease.external());
                    self.lease = Some(lease);
                    Event::Open(addr)
                }
                Ok(Err(failure)) => {
                    crate::note!("router: no port opened ({failure})");
                    Event::Failed(guide(&failure, self.port, self.lan))
                }
                Err(e) => Event::Failed(format!("Opening a port stopped: {e}")),
            };
        }
        loop {
            let Some(at) = self.lease.as_ref().and_then(Lease::renew_at) else {
                return std::future::pending().await;
            };
            tokio::time::sleep_until(at).await;
            let Some(lease) = &mut self.lease else {
                continue;
            };
            match lease.renew().await {
                Ok(true) => return Event::Open(SocketAddr::V4(lease.external())),
                Ok(false) => {}
                Err(e) => {
                    crate::note!("router: renewal failed ({e})");
                    self.lease = None;
                    return Event::Failed(format!(
                        "Your router stopped keeping the port open ({e}). Guests already in \
                         the call stay connected."
                    ));
                }
            }
        }
    }

    /// Removes the mapping, if there is one.
    pub async fn close(self) {
        if let Some(opening) = self.opening {
            opening.abort();
        }
        if let Some(lease) = self.lease {
            lease.close().await;
            crate::note!("router: port closed");
        }
    }
}

/// What to do by hand when the router did not open the port.
pub fn guide(failure: &Failure, port: u16, lan: Option<Ipv4Addr>) -> String {
    match failure {
        Failure::DoubleNat(ip) => format!(
            "Your internet provider shares one public address between customers (your router's \
             is {ip}), so no router setting can let guests in. Use a public veil-node: put it \
             under Settings > Connection > Rendezvous and relay. Guests on your own network \
             can still join."
        ),
        Failure::Unavailable(tried) => {
            let why = if tried.is_empty() {
                String::from("this computer has no IPv4 address on a local network")
            } else {
                failure.to_string()
            };
            let me = lan.map_or_else(
                || String::from("this computer"),
                |ip| format!("this computer ({ip})"),
            );
            format!(
                "Your router did not open a port ({why}). To let guests on the internet reach \
                 you, either turn on UPnP or NAT-PMP in your router's settings, or forward UDP \
                 port {port} to {me}, then set Call port to {port} and add your public IP and \
                 port under Settings > Connection > Public addresses. Guests on your own \
                 network can still join."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use veil_portmap::Protocol;

    use super::*;

    #[test]
    fn the_guide_names_the_port_and_this_computer() {
        let failure = Failure::Unavailable(vec![(Protocol::Upnp, "no router answered".into())]);
        let text = guide(&failure, 7710, Some(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(
            text.contains("UDP port 7710 to this computer (192.168.1.20)"),
            "{text}"
        );
        assert!(text.contains("UPnP: no router answered"), "{text}");
        let none = guide(&Failure::Unavailable(Vec::new()), 7710, None);
        assert!(none.contains("no IPv4 address"), "{none}");
    }

    #[test]
    fn carrier_nat_needs_a_public_node() {
        let text = guide(
            &Failure::DoubleNat(Ipv4Addr::new(100, 64, 3, 4)),
            7710,
            None,
        );
        assert!(
            text.contains("100.64.3.4") && text.contains("veil-node"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn off_does_nothing() {
        assert!(Router::start(false, None, 7710).is_none());
        let mut router = Router::start(true, None, 7710);
        let event = match &mut router {
            Some(r) => r.next().await,
            None => Event::Open(SocketAddr::from(([0, 0, 0, 0], 0))),
        };
        assert!(
            matches!(event, Event::Failed(_)),
            "no LAN address is reported once"
        );
    }
}
