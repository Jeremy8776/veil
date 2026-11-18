//! Opening one UDP port on the home router for the length of a call.
//!
//! Purpose: a guest can reach a host directly only if the host's router lets the first packet
//! in. [`Lease::open`] asks the router to forward the call's UDP port to this machine, trying
//! PCP (RFC 6887), then NAT-PMP (RFC 6886), then UPnP IGD, and returns the address guests can
//! dial. The caller renews the lease at [`Lease::renew_at`] and removes it with
//! [`Lease::close`] when the call ends.
//!
//! Invariants:
//! - One port, UDP only, to this machine's own LAN address. Nothing else is asked for.
//! - Leases are short ([`LIFETIME`]) and renewed while the call runs, so a crash leaves a
//!   mapping that expires on its own. Only a UPnP router that refuses timed leases gets a
//!   permanent one (see the shortcut below).
//! - Only the default gateway, or for UPnP the device that answered and the address it named,
//!   is ever contacted (`upnp`, `udp`). Every step has a time limit; nothing waits for ever.
//! - A router whose own external address is not public sits behind another NAT (usually the
//!   provider's carrier-grade NAT). Its mapping opens nothing, so it is removed and reported
//!   as [`Failure::DoubleNat`].
//!
//! shortcut: a second NAT is spotted only when the router's own external address is private
//! or shared. A router given a public address that is still not the one the internet sees
//! goes unnoticed. Compare with a rendezvous's reflected address once `veil-call` exposes it.
//!
//! IPv6 has no NAT, only the router's firewall: [`Pinhole`] asks the router with PCP to let
//! the call's port through to this machine's public IPv6 address (`pinhole.rs`).
//!
//! shortcut: a UPnP router that allows permanent mappings only (error 725) gets one, removed
//! when the call ends; if Veil crashes it stays until the router restarts or someone removes
//! the "Veil" entry. Clean it up at the next start if that turns out to matter.

pub mod addr;
mod gateway;
mod gateway6;
mod natpmp;
mod pcp;
pub mod pinhole;
mod udp;
pub mod upnp;

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use tokio::time::{Instant, timeout};

pub use pinhole::Pinhole;

/// The lease asked for. Routers may grant less; renewal happens at half of what was granted.
pub const LIFETIME: Duration = Duration::from_secs(20 * 60);
/// Removing a mapping must not hold up leaving the call.
pub const CLOSE_LIMIT: Duration = Duration::from_millis(800);
/// The port PCP and NAT-PMP servers listen on.
const SERVER_PORT: u16 = 5351;
/// The shortest renewal interval, whatever a router grants.
const MIN_RENEW: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Pcp,
    NatPmp,
    Upnp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pcp => "PCP",
            Self::NatPmp => "NAT-PMP",
            Self::Upnp => "UPnP",
        })
    }
}

/// Why no usable port was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No router mapped the port; each way tried and what happened.
    Unavailable(Vec<(Protocol, String)>),
    /// The router mapped it, but its external address is not public, so another NAT sits
    /// in front of it. The mapping was removed.
    DoubleNat(Ipv4Addr),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(tried) => {
                let parts: Vec<String> =
                    tried.iter().map(|(p, why)| format!("{p}: {why}")).collect();
                write!(f, "{}", parts.join("; "))
            }
            Self::DoubleNat(ip) => write!(
                f,
                "the router's internet address {ip} is not public, so another NAT is in front of it"
            ),
        }
    }
}

/// How the mapping was made, with what is needed to renew and remove it.
#[derive(Clone, Debug)]
enum Way {
    Pcp {
        gateway: Ipv4Addr,
        request: pcp::Request,
    },
    NatPmp {
        gateway: Ipv4Addr,
        external_ip: Ipv4Addr,
    },
    Upnp {
        igd: upnp::Igd,
        permanent: bool,
    },
}

/// One open port on the router.
#[derive(Debug)]
pub struct Lease {
    way: Way,
    local: Ipv4Addr,
    port: u16,
    external: SocketAddrV4,
    /// `None` for a permanent UPnP mapping.
    lifetime: Option<Duration>,
    granted: Instant,
}

impl Lease {
    /// Asks the router to forward UDP `port` on its external address to `local:port`.
    pub async fn open(local: Ipv4Addr, port: u16) -> Result<Self, Failure> {
        let gateway = gateway::find(local).await;
        let mut tried = Vec::new();
        let mut lease = None;
        match gateway {
            Some(gateway) => match open_pcp(local, gateway, port).await {
                Ok(found) => lease = Some(found),
                Err(Fallback::NatPmp) => match open_natpmp(local, gateway, port).await {
                    Ok(found) => lease = Some(found),
                    Err(why) => tried.push((Protocol::NatPmp, why)),
                },
                Err(Fallback::Failed(why)) => tried.push((Protocol::Pcp, why)),
            },
            None => tried.push((Protocol::Pcp, String::from("no default gateway found"))),
        }
        if lease.is_none() {
            match open_upnp(local, gateway, port).await {
                Ok(found) => lease = Some(found),
                Err(why) => tried.push((Protocol::Upnp, why)),
            }
        }
        let Some(lease) = lease else {
            return Err(Failure::Unavailable(tried));
        };
        let ip = *lease.external.ip();
        if !addr::is_public(ip) {
            lease.close().await;
            return Err(Failure::DoubleNat(ip));
        }
        Ok(lease)
    }

    /// The address guests can dial.
    pub fn external(&self) -> SocketAddrV4 {
        self.external
    }

    pub fn protocol(&self) -> Protocol {
        match self.way {
            Way::Pcp { .. } => Protocol::Pcp,
            Way::NatPmp { .. } => Protocol::NatPmp,
            Way::Upnp { .. } => Protocol::Upnp,
        }
    }

    /// When to call [`Lease::renew`]: half way through the lease. `None` if it never expires.
    pub fn renew_at(&self) -> Option<Instant> {
        self.lifetime
            .map(|life| self.granted + (life / 2).max(MIN_RENEW))
    }

    /// Asks for the lease again. Returns true if the external address changed, so the caller
    /// can update what it gives out.
    pub async fn renew(&mut self) -> Result<bool, String> {
        let before = self.external;
        let fresh = match &self.way {
            Way::Pcp { gateway, request } => pcp_exchange(self.local, *gateway, *request).await?,
            Way::NatPmp { gateway, .. } => {
                natpmp_map(self.local, *gateway, self.port, self.external.port()).await?
            }
            Way::Upnp { igd, permanent } => {
                let lease = if *permanent { Duration::ZERO } else { LIFETIME };
                upnp::add(igd, self.external.port(), self.local, self.port, lease)
                    .await
                    .map_err(|e| e.text)?;
                (self.external, self.lifetime.unwrap_or(LIFETIME))
            }
        };
        let (mut external, lifetime) = fresh;
        if let Way::NatPmp { external_ip, .. } = &self.way {
            external = SocketAddrV4::new(*external_ip, external.port());
        }
        self.external = external;
        if self.lifetime.is_some() {
            self.lifetime = Some(lifetime);
        }
        self.granted = Instant::now();
        Ok(before != self.external)
    }

    /// Removes the mapping, giving up after [`CLOSE_LIMIT`]. The router drops it anyway when
    /// the lease runs out.
    pub async fn close(self) {
        let job = async {
            match &self.way {
                Way::Pcp { gateway, request } => {
                    let server = SocketAddrV4::new(*gateway, SERVER_PORT);
                    let bytes = request.encode(Duration::ZERO);
                    let _ = udp::exchange(self.local, server, &bytes, |d| request.parse(d)).await;
                }
                Way::NatPmp { gateway, .. } => {
                    let server = SocketAddrV4::new(*gateway, SERVER_PORT);
                    let bytes = natpmp::map_request(self.port, 0, Duration::ZERO);
                    let port = self.port;
                    let _ =
                        udp::exchange(self.local, server, &bytes, |d| natpmp::parse_map(d, port))
                            .await;
                }
                Way::Upnp { igd, .. } => {
                    let _ = upnp::delete(igd, self.external.port()).await;
                }
            }
        };
        let _ = timeout(CLOSE_LIMIT, job).await;
    }
}

enum Fallback {
    NatPmp,
    Failed(String),
}

async fn open_pcp(local: Ipv4Addr, gateway: Ipv4Addr, port: u16) -> Result<Lease, Fallback> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| Fallback::Failed(format!("no randomness: {e}")))?;
    let request = pcp::Request {
        client: local.into(),
        nonce,
        internal_port: port,
        external_port: port,
    };
    let server = SocketAddrV4::new(gateway, SERVER_PORT);
    let bytes = request.encode(LIFETIME);
    let answer = udp::exchange(local, server, &bytes, |d| request.parse(d))
        .await
        .map_err(Fallback::Failed)?;
    match answer {
        // `parse` takes answers in the request's family only, so no IPv6 address comes back.
        None | Some(pcp::Answer::Mapped { external: IpAddr::V6(_), .. }) => {
            Err(Fallback::Failed(String::from("no answer")))
        }
        Some(pcp::Answer::NatPmpOnly) => Err(Fallback::NatPmp),
        Some(pcp::Answer::Refused(code)) => Err(Fallback::Failed(pcp::result_name(code).into())),
        Some(pcp::Answer::Mapped {
            external: IpAddr::V4(external),
            external_port,
            lifetime,
        }) => Ok(Lease {
            // Renewals ask for the port the router gave, as RFC 6887 section 11.2.1 says.
            way: Way::Pcp {
                gateway,
                request: pcp::Request {
                    external_port,
                    ..request
                },
            },
            local,
            port,
            external: SocketAddrV4::new(external, external_port),
            lifetime: Some(lifetime),
            granted: Instant::now(),
        }),
    }
}

/// Renews (or re-asks) a PCP mapping with the same nonce.
async fn pcp_exchange(
    local: Ipv4Addr,
    gateway: Ipv4Addr,
    request: pcp::Request,
) -> Result<(SocketAddrV4, Duration), String> {
    let server = SocketAddrV4::new(gateway, SERVER_PORT);
    let bytes = request.encode(LIFETIME);
    match udp::exchange(local, server, &bytes, |d| request.parse(d)).await? {
        Some(pcp::Answer::Mapped {
            external: IpAddr::V4(external),
            external_port,
            lifetime,
        }) => Ok((SocketAddrV4::new(external, external_port), lifetime)),
        Some(pcp::Answer::Refused(code)) => Err(pcp::result_name(code).into()),
        Some(pcp::Answer::Mapped { .. } | pcp::Answer::NatPmpOnly) | None => {
            Err(String::from("no answer"))
        }
    }
}

async fn open_natpmp(local: Ipv4Addr, gateway: Ipv4Addr, port: u16) -> Result<Lease, String> {
    let server = SocketAddrV4::new(gateway, SERVER_PORT);
    let external_ip = match udp::exchange(
        local,
        server,
        &natpmp::ADDRESS_REQUEST,
        natpmp::parse_address,
    )
    .await?
    {
        Some(Ok(ip)) => ip,
        Some(Err(code)) => return Err(natpmp::result_name(code).into()),
        None => return Err(String::from("no answer")),
    };
    let (mapped, lifetime) = natpmp_map(local, gateway, port, port).await?;
    Ok(Lease {
        way: Way::NatPmp {
            gateway,
            external_ip,
        },
        local,
        port,
        external: SocketAddrV4::new(external_ip, mapped.port()),
        lifetime: Some(lifetime),
        granted: Instant::now(),
    })
}

/// A NAT-PMP mapping request. The address in the result is unspecified: NAT-PMP gives the
/// external address in a separate answer.
async fn natpmp_map(
    local: Ipv4Addr,
    gateway: Ipv4Addr,
    port: u16,
    external_port: u16,
) -> Result<(SocketAddrV4, Duration), String> {
    let server = SocketAddrV4::new(gateway, SERVER_PORT);
    let bytes = natpmp::map_request(port, external_port, LIFETIME);
    match udp::exchange(local, server, &bytes, |d| natpmp::parse_map(d, port)).await? {
        Some(Ok((ext, lifetime))) => Ok((SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, ext), lifetime)),
        Some(Err(code)) => Err(natpmp::result_name(code).into()),
        None => Err(String::from("no answer")),
    }
}

async fn open_upnp(local: Ipv4Addr, gateway: Option<Ipv4Addr>, port: u16) -> Result<Lease, String> {
    let igd = upnp::discover(local, gateway).await?;
    let external_ip = upnp::external_ip(&igd).await.map_err(|e| e.text)?;
    let mut permanent = false;
    let mut last = String::new();
    for external_port in [port, spare_port(), spare_port()] {
        let lease = if permanent { Duration::ZERO } else { LIFETIME };
        match upnp::add(&igd, external_port, local, port, lease).await {
            Ok(()) => {
                return Ok(Lease {
                    way: Way::Upnp { igd, permanent },
                    local,
                    port,
                    external: SocketAddrV4::new(external_ip, external_port),
                    lifetime: (!permanent).then_some(LIFETIME),
                    granted: Instant::now(),
                });
            }
            Err(e) if e.code == Some(upnp::ONLY_PERMANENT_LEASES) && !permanent => {
                // Ask once more for the same port, without a lease.
                permanent = true;
                match upnp::add(&igd, external_port, local, port, Duration::ZERO).await {
                    Ok(()) => {
                        return Ok(Lease {
                            way: Way::Upnp { igd, permanent },
                            local,
                            port,
                            external: SocketAddrV4::new(external_ip, external_port),
                            lifetime: None,
                            granted: Instant::now(),
                        });
                    }
                    Err(e) if e.code == Some(upnp::CONFLICT) => last = e.text,
                    Err(e) => return Err(e.text),
                }
            }
            Err(e) if e.code == Some(upnp::CONFLICT) => last = e.text,
            Err(e) => return Err(e.text),
        }
    }
    Err(last)
}

/// A random port in the dynamic range, for when the call's own port is taken on the router.
fn spare_port() -> u16 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    49152 + u16::from_be_bytes(b) % 16384
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spare_ports_are_dynamic() {
        for _ in 0..100 {
            assert!(spare_port() >= 49152);
        }
    }

    #[test]
    fn failures_read_plainly() {
        let tried = Failure::Unavailable(vec![
            (Protocol::Pcp, "no answer".into()),
            (Protocol::Upnp, "no router answered".into()),
        ]);
        assert_eq!(
            tried.to_string(),
            "PCP: no answer; UPnP: no router answered"
        );
        assert!(
            Failure::DoubleNat(Ipv4Addr::new(100, 64, 0, 9))
                .to_string()
                .contains("100.64.0.9")
        );
    }

    /// Run by hand on a network with a router that maps ports:
    /// `cargo test -j 8 -p veil-portmap real_router -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "talks to this network's router"]
    async fn real_router() {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok();
        let local = socket
            .and_then(|s| s.connect("192.0.2.1:9").ok().map(|()| s))
            .and_then(|s| s.local_addr().ok())
            .and_then(|a| match a.ip() {
                std::net::IpAddr::V4(ip) => Some(ip),
                std::net::IpAddr::V6(_) => None,
            });
        let Some(local) = local else {
            println!("no IPv4 LAN address");
            return;
        };
        println!("gateway: {:?}", gateway::find(local).await);
        match Lease::open(local, 47710).await {
            Ok(mut lease) => {
                println!("{} mapped {}", lease.protocol(), lease.external());
                println!("renewed, changed: {:?}", lease.renew().await);
                lease.close().await;
                println!("closed");
            }
            Err(e) => println!("no mapping: {e}"),
        }
    }

    /// The IPv6 twin of `real_router`:
    /// `cargo test -j 8 -p veil-portmap real_router_ipv6 -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "talks to this network's router"]
    async fn real_router_ipv6() {
        let socket = std::net::UdpSocket::bind("[::]:0").ok();
        let local = socket
            .and_then(|s| s.connect("[2001:db8::1]:9").ok().map(|()| s))
            .and_then(|s| s.local_addr().ok())
            .and_then(|a| match a.ip() {
                std::net::IpAddr::V6(ip) if !ip.is_unicast_link_local() => Some(ip),
                _ => None,
            });
        let Some(local) = local else {
            println!("no public IPv6 address");
            return;
        };
        println!("router: {:?}", gateway6::find(SERVER_PORT).await);
        match Pinhole::open(local, 47710).await {
            Ok(mut pinhole) => {
                println!("PCP opened {}", pinhole.external());
                println!("renewed, changed: {:?}", pinhole.renew().await);
                pinhole.close().await;
                println!("closed");
            }
            Err(e) => println!("no pinhole: {e}"),
        }
    }
}
