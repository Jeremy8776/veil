//! The host's router port for one call: opened when asked for, renewed, removed at the end.
//!
//! Purpose: wraps `veil_portmap::Lease` for `host.rs`. Opening takes up to a few seconds, so
//! the call starts at once with its LAN addresses and [`Router::next`] reports the public
//! address when the router gives one; the host then puts it first in a fresh invite. If the
//! router does not open the port, [`guide`] says what the person can do by hand.
//!
//! A host running its own rendezvous opens two ports, one for the call and one for the
//! rendezvous. [`PortStatus`] adds them up, so the host is told one thing and gives out a
//! public invite only once both have answered: a link made in between would let guests reach
//! the host but not each other.
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

use crate::model::RouterPort;

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

/// How far one port has got.
enum Side {
    NotAsked,
    Opening,
    Open(SocketAddr),
    Failed(String),
}

impl Side {
    fn start(router: Option<&Router>) -> Self {
        if router.is_some() { Self::Opening } else { Self::NotAsked }
    }

    fn after(event: Event) -> Self {
        match event {
            Event::Open(addr) => Self::Open(addr),
            Event::Failed(text) => Self::Failed(text),
        }
    }
}

/// The call's port and the host's rendezvous port, added up for the host and its invites.
pub struct PortStatus {
    call: Side,
    rendezvous: Side,
}

impl PortStatus {
    pub fn new(call: Option<&Router>, rendezvous: Option<&Router>) -> Self {
        Self {
            call: Side::start(call),
            rendezvous: Side::start(rendezvous),
        }
    }

    pub fn call_changed(&mut self, event: Event) {
        self.call = Side::after(event);
    }

    pub fn rendezvous_changed(&mut self, event: Event) {
        self.rendezvous = Side::after(event);
    }

    /// Whether the router has answered for every port asked for.
    pub fn settled(&self) -> bool {
        !matches!(self.call, Side::Opening) && !matches!(self.rendezvous, Side::Opening)
    }

    /// What the host is shown.
    pub fn view(&self) -> RouterPort {
        match (&self.call, &self.rendezvous) {
            (Side::Opening, _) | (_, Side::Opening) => RouterPort::Opening,
            (Side::NotAsked, _) => RouterPort::Off,
            (Side::Failed(guide), _) => RouterPort::Failed(guide.clone()),
            (Side::Open(addr), Side::Failed(_)) => RouterPort::GuestsApart(addr.to_string()),
            (Side::Open(addr), _) => RouterPort::Open(addr.to_string()),
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
                 port under Settings > Connection > Public addresses. They will reach you, \
                 but not each other without a public rendezvous. Guests on your own \
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
    fn both_ports_must_answer_before_the_host_is_told() {
        let addr = SocketAddr::from(([203, 0, 113, 9], 7710));
        let mut ports = PortStatus {
            call: Side::Opening,
            rendezvous: Side::Opening,
        };
        ports.call_changed(Event::Open(addr));
        assert!(!ports.settled(), "the rendezvous port has not answered");
        assert_eq!(ports.view(), RouterPort::Opening);
        ports.rendezvous_changed(Event::Open(addr));
        assert!(ports.settled());
        assert_eq!(ports.view(), RouterPort::Open(addr.to_string()));
    }

    #[test]
    fn a_closed_rendezvous_port_is_shown() {
        let addr = SocketAddr::from(([203, 0, 113, 9], 7710));
        let mut ports = PortStatus {
            call: Side::Open(addr),
            rendezvous: Side::Opening,
        };
        ports.rendezvous_changed(Event::Failed(String::from("no")));
        assert!(ports.settled());
        assert_eq!(ports.view(), RouterPort::GuestsApart(addr.to_string()));
        ports.call_changed(Event::Failed(String::from("guide")));
        assert_eq!(ports.view(), RouterPort::Failed(String::from("guide")));
    }

    #[test]
    fn a_configured_rendezvous_needs_only_the_call_port() {
        let addr = SocketAddr::from(([203, 0, 113, 9], 7710));
        let mut ports = PortStatus {
            call: Side::Opening,
            rendezvous: Side::NotAsked,
        };
        assert!(!ports.settled());
        ports.call_changed(Event::Open(addr));
        assert!(ports.settled());
        assert_eq!(ports.view(), RouterPort::Open(addr.to_string()));
        let off = PortStatus::new(None, None);
        assert!(off.settled());
        assert_eq!(off.view(), RouterPort::Off);
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
