//! Control messages on a reliable QUIC stream.
//!
//! Wire format: u32 big-endian length, then a postcard-encoded [`Control`]. Lengths over
//! [`MAX_CONTROL_LEN`] are refused before any allocation.

use serde::{Deserialize, Serialize};

use crate::NetError;

pub const MAX_CONTROL_LEN: usize = 64 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Joiner to host: the room and one-time token from the invite link.
    Hello { room_id: [u8; 16], join_token: [u8; 16] },
    /// Host to joiner: admitted from the lobby. Group keys follow separately.
    Admitted,
    /// Host to joiner: not admitted.
    Refused(RefuseReason),
    /// Round-trip measurement.
    Ping(u64),
    Pong(u64),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    UnknownRoom,
    TokenUsed,
    DeclinedByHost,
    RoomFull,
}

/// Encodes a message with its length prefix.
pub fn encode(message: &Control) -> Result<Vec<u8>, NetError> {
    let body = postcard::to_allocvec(message).map_err(|_| NetError::Protocol("encode"))?;
    if body.len() > MAX_CONTROL_LEN {
        return Err(NetError::TooLarge);
    }
    let len = u32::try_from(body.len()).map_err(|_| NetError::TooLarge)?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Checks a length prefix before reading the body.
pub fn body_len(prefix: [u8; 4]) -> Result<usize, NetError> {
    let len = usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| NetError::TooLarge)?;
    if len > MAX_CONTROL_LEN {
        return Err(NetError::TooLarge);
    }
    Ok(len)
}

pub fn decode(body: &[u8]) -> Result<Control, NetError> {
    postcard::from_bytes(body).map_err(|_| NetError::Protocol("decode"))
}

pub async fn write(stream: &mut quinn::SendStream, message: &Control) -> Result<(), NetError> {
    let bytes = encode(message)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))
}

pub async fn read(stream: &mut quinn::RecvStream) -> Result<Control, NetError> {
    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))?;
    let mut body = vec![0u8; body_len(prefix)?];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))?;
    decode(&body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let message = Control::Hello {
            room_id: [1; 16],
            join_token: [2; 16],
        };
        let bytes = encode(&message).unwrap();
        let len = body_len(bytes[..4].try_into().unwrap()).unwrap();
        assert_eq!(len, bytes.len() - 4);
        assert_eq!(decode(&bytes[4..]).unwrap(), message);
    }

    #[test]
    fn oversized_length_is_refused_before_reading() {
        let prefix = u32::try_from(MAX_CONTROL_LEN + 1).unwrap().to_be_bytes();
        assert!(matches!(body_len(prefix), Err(NetError::TooLarge)));
        assert!(matches!(body_len([0xff; 4]), Err(NetError::TooLarge)));
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode(&[0xff, 0xff, 0xff]).is_err());
    }
}
