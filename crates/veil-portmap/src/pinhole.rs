//! An IPv6 pinhole in the home router's firewall for the length of a call.
//!
//! Purpose: IPv6 has no NAT, so a guest can dial a host's public IPv6 address directly, but
//! most home routers' firewalls drop packets nobody inside asked for. [`Pinhole::open`] asks
//! the default IPv6 router, with PCP (RFC 6887 section 11), to let UDP to this machine's
//! address and the call's port through. It works like [`crate::Lease`]: renew at
//! [`Pinhole::renew_at`], remove with [`Pinhole::close`].
//!
//! Invariants:
//! - One port, UDP only, to this machine's own public IPv6 address, which the request is
//!   sent from (PCP refuses a request whose stated address is not its source).
//! - Only the default IPv6 router is contacted (`gateway6`). Every step has a time limit.
//! - Short leases ([`crate::LIFETIME`]), renewed with the same nonce, deleted at the end.
//!
//! shortcut: PCP only. Many home routers offer no IPv6 pinholes at all, and some offer them
//! through UPnP IGDv2's WANIPv6FirewallControl instead. Without a pinhole an IPv6 call still
//! works by simultaneous dial through a rendezvous. Add UPnP pinholes if tests show routers
//! that have them and block that dial.

use std::net::{Ipv6Addr, SocketAddrV6};
use std::time::Duration;

use tokio::time::{Instant, timeout};

use crate::{CLOSE_LIMIT, LIFETIME, MIN_RENEW, SERVER_PORT, gateway6, pcp, udp};

/// One open pinhole.
#[derive(Debug)]
pub struct Pinhole {
    router: SocketAddrV6,
    request: pcp::Request,
    local: Ipv6Addr,
    external: SocketAddrV6,
    lifetime: Duration,
    granted: Instant,
}

impl Pinhole {
    /// Asks the router to let UDP to `local` at `port` through its firewall. `local` must be
    /// this machine's public IPv6 address (`veil_net::family::global_ipv6`).
    pub async fn open(local: Ipv6Addr, port: u16) -> Result<Self, String> {
        let router = gateway6::find(SERVER_PORT)
            .await
            .ok_or_else(|| String::from("no default IPv6 router found"))?;
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| format!("no randomness: {e}"))?;
        let request = pcp::Request {
            client: local.into(),
            nonce,
            internal_port: port,
            external_port: port,
        };
        let (external, lifetime) = ask(local, router, request, LIFETIME).await?;
        Ok(Self {
            router,
            request,
            local,
            external,
            lifetime,
            granted: Instant::now(),
        })
    }

    /// The address guests dial: normally this machine's own address and port.
    pub fn external(&self) -> SocketAddrV6 {
        self.external
    }

    /// When to call [`Pinhole::renew`]: half way through the lease.
    pub fn renew_at(&self) -> Instant {
        self.granted + (self.lifetime / 2).max(MIN_RENEW)
    }

    /// Asks for the pinhole again. Returns true if its address changed.
    pub async fn renew(&mut self) -> Result<bool, String> {
        let (external, lifetime) = ask(self.local, self.router, self.request, LIFETIME).await?;
        let changed = external != self.external;
        self.external = external;
        self.lifetime = lifetime;
        self.granted = Instant::now();
        Ok(changed)
    }

    /// Removes the pinhole, giving up after [`CLOSE_LIMIT`]. The router drops it anyway when
    /// the lease runs out.
    pub async fn close(self) {
        let _ = timeout(
            CLOSE_LIMIT,
            ask(self.local, self.router, self.request, Duration::ZERO),
        )
        .await;
    }
}

async fn ask(
    local: Ipv6Addr,
    router: SocketAddrV6,
    request: pcp::Request,
    lifetime: Duration,
) -> Result<(SocketAddrV6, Duration), String> {
    let bytes = request.encode(lifetime);
    match udp::exchange(local, router, &bytes, |d| request.parse(d)).await? {
        Some(pcp::Answer::Mapped {
            external: std::net::IpAddr::V6(ip),
            external_port,
            lifetime,
        }) => Ok((SocketAddrV6::new(ip, external_port, 0, 0), lifetime)),
        Some(pcp::Answer::Refused(code)) => Err(pcp::result_name(code).into()),
        Some(pcp::Answer::NatPmpOnly) => Err(String::from("the router speaks NAT-PMP only")),
        Some(pcp::Answer::Mapped { .. }) | None => Err(String::from("no answer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A router on this machine's loopback that opens every pinhole asked for.
    #[tokio::test]
    async fn a_pinhole_is_asked_for_from_the_public_address() -> Result<(), String> {
        let Ok(router) = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await else {
            eprintln!("no IPv6 loopback; skipped");
            return Ok(());
        };
        let std::net::SocketAddr::V6(at) = router.local_addr().map_err(|e| e.to_string())? else {
            return Err("not IPv6".into());
        };
        tokio::spawn(async move {
            let mut buf = [0u8; 1100];
            while let Ok((n, from)) = router.recv_from(&mut buf).await {
                let mut answer = buf[..n].to_vec();
                answer[1] |= 0x80;
                answer[8..24].fill(0);
                // Firewall only: the external address is the client's own.
                let client = buf[8..24].to_vec();
                answer[44..60].copy_from_slice(&client);
                let _ = router.send_to(&answer, from).await;
            }
        });
        let request = pcp::Request {
            client: Ipv6Addr::LOCALHOST.into(),
            nonce: [5; 12],
            internal_port: 7710,
            external_port: 7710,
        };
        let (external, lifetime) = ask(Ipv6Addr::LOCALHOST, at, request, LIFETIME).await?;
        assert_eq!(external, SocketAddrV6::new(Ipv6Addr::LOCALHOST, 7710, 0, 0));
        assert_eq!(lifetime, LIFETIME);
        Ok(())
    }
}
