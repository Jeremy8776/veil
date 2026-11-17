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

use std::net::{Ipv4Addr, Ipv6Addr};
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
    pub client: Ipv4Addr,
    pub nonce: [u8; 12],
    pub internal_port: u16,
    /// The external port asked for; the router may give another.
    pub external_port: u16,
}

/// What the router said to a MAP request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Mapped {
        external: Ipv4Addr,
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
        out[8..24].copy_from_slice(&self.client.to_ipv6_mapped().octets());
        out[24..36].copy_from_slice(&self.nonce);
        out[36] = UDP;
        out[40..42].copy_from_slice(&self.internal_port.to_be_bytes());
        out[42..44].copy_from_slice(&self.external_port.to_be_bytes());
        // No preferred external address: the IPv4-mapped all-zeros address.
        out[44..60].copy_from_slice(&Ipv4Addr::UNSPECIFIED.to_ipv6_mapped().octets());
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
        let external = mapped_v4(&data[44..60])?;
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

fn mapped_v4(b: &[u8]) -> Option<Ipv4Addr> {
    let octets: [u8; 16] = b.try_into().ok()?;
    Ipv6Addr::from(octets).to_ipv4_mapped()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            client: Ipv4Addr::new(192, 168, 1, 20),
            nonce: [7; 12],
            internal_port: 7710,
            external_port: 7710,
        }
    }

    /// The router's answer: the request turned round, as RFC 6887 section 11.4 describes.
    fn answer(req: &Request, result: u8, external: Ipv4Addr, port: u16) -> Vec<u8> {
        let mut out = req.encode(Duration::from_secs(1200)).to_vec();
        out[1] = RESPONSE | OP_MAP;
        out[3] = result;
        out[8..24].fill(0); // epoch and reserved
        out[42..44].copy_from_slice(&port.to_be_bytes());
        out[44..60].copy_from_slice(&external.to_ipv6_mapped().octets());
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
                external: ext,
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
    fn nat_pmp_routers_say_so() {
        assert_eq!(
            request().parse(&[0, 0x81, 0, 1, 0, 0, 0, 0]),
            Some(Answer::NatPmpOnly)
        );
        assert_eq!(request().parse(&[0, 0x81, 0, 0]), None);
    }
}
