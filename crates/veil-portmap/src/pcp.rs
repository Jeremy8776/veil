//! PCP (RFC 6887): MAP requests for one UDP port, and their answers.
//!
//! Purpose: the newest of the three ways to ask a router for a port, and the only one with a
//! nonce, so an answer is tied to its request. A router that speaks only NAT-PMP answers a
//! PCP request with NAT-PMP's "unsupported version", which tells Veil to use NAT-PMP at once.
//!
//! Invariants:
//! - An answer counts only if it is a MAP response with this request's nonce, protocol and
//!   internal port (`parse`). Anything else is ignored.
//! - The same nonce renews and then deletes the mapping (lifetime 0), as the RFC requires.
//! - IPv4 requests map a port through the router's NAT; IPv6 requests open a pinhole in its
//!   firewall (RFC 6887 section 11). An answer counts only in the request's own family.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

const VERSION: u8 = 2;
const OP_MAP: u8 = 1;
const RESPONSE: u8 = 0x80;
const UDP: u8 = 17;
/// Header (24 bytes) plus MAP payload (36 bytes).
const LEN: usize = 60;

/// A MAP request's fixed fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// This machine's address: its LAN IPv4 address, or its public IPv6 address for a
    /// pinhole. It must be the address the request is sent from.
    pub client: IpAddr,
    pub nonce: [u8; 12],
    pub internal_port: u16,
    /// The external port asked for; the router may give another.
    pub external_port: u16,
}

/// What the router said to a MAP request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Mapped {
        external: IpAddr,
        external_port: u16,
        lifetime: Duration,
    },
    /// A PCP result code other than success.
    Refused(u8),
    /// The router speaks NAT-PMP (version 0) only.
    NatPmpOnly,
}

impl Request {
    /// The request bytes for a mapping of `lifetime` (zero deletes it).
    pub fn encode(&self, lifetime: Duration) -> [u8; LEN] {
        let mut out = [0u8; LEN];
        out[0] = VERSION;
        out[1] = OP_MAP;
        out[4..8].copy_from_slice(&secs(lifetime).to_be_bytes());
        out[8..24].copy_from_slice(&as_v6(self.client).octets());
        out[24..36].copy_from_slice(&self.nonce);
        out[36] = UDP;
        out[40..42].copy_from_slice(&self.internal_port.to_be_bytes());
        out[42..44].copy_from_slice(&self.external_port.to_be_bytes());
        // No preferred external address: the all-zeros address of the request's family.
        let any = match self.client {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        out[44..60].copy_from_slice(&as_v6(any).octets());
        out
    }

    /// Reads an answer to this request. `None` for anything that is not one.
    pub fn parse(&self, data: &[u8]) -> Option<Answer> {
        let version = *data.first()?;
        if version == 0 {
            // NAT-PMP's reply to an unknown version: opcode 128 + ours, result 1.
            return (data.len() >= 4 && data[1] == RESPONSE | OP_MAP && data[3] == 1)
                .then_some(Answer::NatPmpOnly);
        }
        if version != VERSION || data.len() < LEN || data[1] != RESPONSE | OP_MAP {
            return None;
        }
        let result = data[3];
        // An error may come back without our payload echoed; trust it only with our nonce.
        if data[24..36] != self.nonce
            || data[36] != UDP
            || be16(&data[40..42]) != self.internal_port
        {
            return None;
        }
        if result != 0 {
            return Some(Answer::Refused(result));
        }
        let lifetime = Duration::from_secs(u64::from(be32(&data[4..8])));
        let external = external(&data[44..60], self.client.is_ipv4())?;
        Some(Answer::Mapped {
            external,
            external_port: be16(&data[42..44]),
            lifetime,
        })
    }
}

/// The RFC's name for a result code, for the log and the guide.
pub fn result_name(code: u8) -> &'static str {
    match code {
        1 => "unsupported version",
        2 => "not authorised",
        3 => "malformed request",
        4 => "unsupported opcode",
        7 => "malformed option",
        8 => "network failure",
        9 => "no resources",
        10 => "unsupported protocol",
        11 => "user quota exceeded",
        12 => "cannot provide that external port",
        13 => "address mismatch",
        14 => "excessive remote peers",
        _ => "refused",
    }
}

fn secs(d: Duration) -> u32 {
    u32::try_from(d.as_secs()).unwrap_or(u32::MAX)
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// PCP's 16-byte address field: IPv4 as an IPv4-mapped address.
fn as_v6(ip: IpAddr) -> Ipv6Addr {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    }
}

/// The assigned external address, only in the request's family.
fn external(b: &[u8], v4: bool) -> Option<IpAddr> {
    let octets: [u8; 16] = b.try_into().ok()?;
    let ip = Ipv6Addr::from(octets);
    match ip.to_ipv4_mapped() {
        Some(mapped) if v4 => Some(IpAddr::V4(mapped)),
        None if !v4 => Some(IpAddr::V6(ip)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            client: Ipv4Addr::new(192, 168, 1, 20).into(),
            nonce: [7; 12],
            internal_port: 7710,
            external_port: 7710,
        }
    }

    /// The router's answer: the request turned round, as RFC 6887 section 11.4 describes.
    fn answer(req: &Request, result: u8, external: impl Into<IpAddr>, port: u16) -> Vec<u8> {
        let mut out = req.encode(Duration::from_secs(1200)).to_vec();
        out[1] = RESPONSE | OP_MAP;
        out[3] = result;
        out[8..24].fill(0); // epoch and reserved
        out[42..44].copy_from_slice(&port.to_be_bytes());
        out[44..60].copy_from_slice(&as_v6(external.into()).octets());
        out
    }

    #[test]
    fn request_layout() {
        let bytes = request().encode(Duration::from_secs(1200));
        assert_eq!(&bytes[..2], &[2, 1]);
        assert_eq!(&bytes[4..8], &1200u32.to_be_bytes());
        assert_eq!(&bytes[18..24], &[0xff, 0xff, 192, 168, 1, 20]);
        assert_eq!(bytes[36], 17);
        assert_eq!(&bytes[40..44], &[0x1e, 0x1e, 0x1e, 0x1e]);
    }

    #[test]
    fn a_matching_answer_maps() {
        let req = request();
        let ext = Ipv4Addr::new(81, 2, 69, 160);
        assert_eq!(
            req.parse(&answer(&req, 0, ext, 40000)),
            Some(Answer::Mapped {
                external: ext.into(),
                external_port: 40000,
                lifetime: Duration::from_secs(1200),
            })
        );
        assert_eq!(
            req.parse(&answer(&req, 2, ext, 0)),
            Some(Answer::Refused(2))
        );
    }

    #[test]
    fn answers_to_someone_else_are_ignored() {
        let req = request();
        let mut other = answer(&req, 0, Ipv4Addr::new(81, 2, 69, 160), 7710);
        other[24] ^= 1;
        assert_eq!(req.parse(&other), None, "wrong nonce");
        let mut port = answer(&req, 0, Ipv4Addr::new(81, 2, 69, 160), 7710);
        port[41] ^= 1;
        assert_eq!(req.parse(&port), None, "wrong internal port");
        assert_eq!(
            req.parse(&req.encode(Duration::ZERO)),
            None,
            "our own request"
        );
        assert_eq!(req.parse(&[2, 0x81, 0]), None, "short");
    }

    #[test]
    fn an_ipv6_pinhole_asks_from_and_for_the_public_address() {
        let me: Ipv6Addr = "2a00:23c5:1234:5600::1".parse().unwrap_or(Ipv6Addr::LOCALHOST);
        let req = Request {
            client: me.into(),
            ..request()
        };
        let bytes = req.encode(Duration::from_secs(1200));
        assert_eq!(&bytes[8..24], &me.octets());
        assert_eq!(&bytes[44..60], &[0u8; 16], "no preferred external address");
        assert_eq!(
            req.parse(&answer(&req, 0, me, 7710)),
            Some(Answer::Mapped {
                external: me.into(),
                external_port: 7710,
                lifetime: Duration::from_secs(1200),
            })
        );
        // An IPv4 answer to an IPv6 request, or the other way round, is not ours.
        let v4 = Ipv4Addr::new(81, 2, 69, 160);
        assert_eq!(req.parse(&answer(&req, 0, v4, 7710)), None);
        assert_eq!(request().parse(&answer(&request(), 0, me, 7710)), None);
    }

    #[test]
    fn nat_pmp_routers_say_so() {
        assert_eq!(
            request().parse(&[0, 0x81, 0, 1, 0, 0, 0, 0]),
            Some(Answer::NatPmpOnly)
        );
        assert_eq!(request().parse(&[0, 0x81, 0, 0]), None);
    }
}
