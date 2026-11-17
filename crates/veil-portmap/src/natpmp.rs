//! NAT-PMP (RFC 6886): the router's external address, and mappings for one UDP port.
//!
//! Purpose: Apple's protocol, which PCP replaced; many routers still speak only this one.
//! Unlike PCP it has no nonce, so an answer is matched by opcode and internal port only. The
//! socket is connected to the gateway (`udp.rs`), so only the gateway can answer.

use std::net::Ipv4Addr;
use std::time::Duration;

const VERSION: u8 = 0;
const OP_ADDRESS: u8 = 0;
const OP_MAP_UDP: u8 = 1;
const RESPONSE: u8 = 0x80;

/// The request for the router's external address.
pub const ADDRESS_REQUEST: [u8; 2] = [VERSION, OP_ADDRESS];

/// A result: the value, or a NAT-PMP result code other than success.
pub type Reply<T> = Result<T, u16>;

/// Reads the answer to [`ADDRESS_REQUEST`].
pub fn parse_address(data: &[u8]) -> Option<Reply<Ipv4Addr>> {
    if data.len() < 12 || data[0] != VERSION || data[1] != RESPONSE | OP_ADDRESS {
        return None;
    }
    match be16(&data[2..4]) {
        0 => Some(Ok(Ipv4Addr::new(data[8], data[9], data[10], data[11]))),
        code => Some(Err(code)),
    }
}

/// A UDP mapping request for `internal_port`, asking for `external_port`, for `lifetime`
/// (zero deletes it, and then the external port must be zero too).
pub fn map_request(internal_port: u16, external_port: u16, lifetime: Duration) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0] = VERSION;
    out[1] = OP_MAP_UDP;
    out[4..6].copy_from_slice(&internal_port.to_be_bytes());
    out[6..8].copy_from_slice(&external_port.to_be_bytes());
    let secs = u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX);
    out[8..12].copy_from_slice(&secs.to_be_bytes());
    out
}

/// Reads the answer to a mapping request for `internal_port`: the external port given and
/// the lifetime granted.
pub fn parse_map(data: &[u8], internal_port: u16) -> Option<Reply<(u16, Duration)>> {
    if data.len() < 16 || data[0] != VERSION || data[1] != RESPONSE | OP_MAP_UDP {
        return None;
    }
    if be16(&data[8..10]) != internal_port {
        return None;
    }
    match be16(&data[2..4]) {
        0 => {
            let lifetime = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
            Some(Ok((
                be16(&data[10..12]),
                Duration::from_secs(u64::from(lifetime)),
            )))
        }
        code => Some(Err(code)),
    }
}

/// The RFC's name for a result code.
pub fn result_name(code: u16) -> &'static str {
    match code {
        1 => "unsupported version",
        2 => "not authorised (turned off on the router)",
        3 => "network failure",
        4 => "out of resources",
        5 => "unsupported opcode",
        _ => "refused",
    }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_address() {
        let ok = [0, 128, 0, 0, 0, 0, 1, 0, 81, 2, 69, 160];
        assert_eq!(parse_address(&ok), Some(Ok(Ipv4Addr::new(81, 2, 69, 160))));
        let refused = [0, 128, 0, 2, 0, 0, 1, 0, 0, 0, 0, 0];
        assert_eq!(parse_address(&refused), Some(Err(2)));
        assert_eq!(parse_address(&ok[..11]), None);
        assert_eq!(parse_address(&[0, 129, 0, 0, 0, 0, 1, 0, 1, 2, 3, 4]), None);
    }

    #[test]
    fn mapping() {
        let req = map_request(7710, 7710, Duration::from_secs(1200));
        assert_eq!(req, [0, 1, 0, 0, 0x1e, 0x1e, 0x1e, 0x1e, 0, 0, 4, 0xb0]);
        let mut answer = [0u8; 16];
        answer[..2].copy_from_slice(&[0, 129]);
        answer[8..10].copy_from_slice(&7710u16.to_be_bytes());
        answer[10..12].copy_from_slice(&40000u16.to_be_bytes());
        answer[12..16].copy_from_slice(&600u32.to_be_bytes());
        assert_eq!(
            parse_map(&answer, 7710),
            Some(Ok((40000, Duration::from_secs(600))))
        );
        assert_eq!(parse_map(&answer, 7711), None, "another port's answer");
        answer[3] = 4;
        assert_eq!(parse_map(&answer, 7710), Some(Err(4)));
    }
}
