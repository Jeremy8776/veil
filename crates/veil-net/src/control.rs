//! Framed messages on a reliable QUIC stream: the call's [`Control`] messages, and any other
//! protocol that rides a link (for example rendezvous).
//!
//! Wire format: u32 big-endian length, then a postcard-encoded message. Lengths over
//! [`MAX_CONTROL_LEN`] are refused before any allocation.

use serde::de::DeserializeOwned;
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
    /// Joiner to host, after `Admitted`: an MLS key package signed by the joiner's device key.
    KeyPackage(Vec<u8>),
    /// Host to joiner: the MLS Welcome that carries the group's keys.
    Welcome(Vec<u8>),
    /// Host to existing members: an MLS commit that starts the next epoch.
    Commit(Vec<u8>),
    /// Member to member, first message: this link carries the call's media. The receiver
    /// still checks that the link's pinned key belongs to a member.
    Media { room_id: [u8; 16] },
    /// Host to members, after each epoch's commit: the signed layout for that epoch.
    Plan(Vec<u8>),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    UnknownRoom,
    TokenUsed,
    DeclinedByHost,
    RoomFull,
}

/// Encodes a message with its length prefix.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, NetError> {
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

pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, NetError> {
    postcard::from_bytes(body).map_err(|_| NetError::Protocol("decode"))
}

pub async fn write<T: Serialize>(stream: &mut quinn::SendStream, message: &T) -> Result<(), NetError> {
    let bytes = encode(message)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))
}

pub async fn read<T: DeserializeOwned>(stream: &mut quinn::RecvStream) -> Result<T, NetError> {
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
        assert_eq!(decode::<Control>(&bytes[4..]).unwrap(), message);
    }

    #[test]
    fn oversized_length_is_refused_before_reading() {
        let prefix = u32::try_from(MAX_CONTROL_LEN + 1).unwrap().to_be_bytes();
        assert!(matches!(body_len(prefix), Err(NetError::TooLarge)));
        assert!(matches!(body_len([0xff; 4]), Err(NetError::TooLarge)));
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode::<Control>(&[0xff, 0xff, 0xff]).is_err());
    }
}
