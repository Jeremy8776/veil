//! Framed messages on a reliable QUIC stream: the call's [`Control`] messages, and any other
//! protocol that rides a link (for example rendezvous).
//!
//! Wire format: u32 big-endian length, then a postcard-encoded message. Lengths over
//! [`MAX_CONTROL_LEN`] are refused before any allocation.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::NetError;

pub const MAX_CONTROL_LEN: usize = 64 * 1024;
/// The largest sealed app message (names, chat) a member or forwarder passes on. Larger ones
/// are dropped unread.
pub const MAX_APP_LEN: usize = 16 * 1024;
/// The most removed or declined device keys a handover passes to the new host.
pub const MAX_HANDOVER_REMOVED: usize = 64;
/// The longest signed contact (the new host's addresses) a handover carries.
pub const MAX_CONTACT_LEN: usize = 4 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Joiner to host: the room and one-time token from the invite link.
    Hello {
        room_id: [u8; 16],
        join_token: [u8; 16],
    },
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
    Media {
        room_id: [u8; 16],
    },
    /// Host to members, after each epoch's commit: the signed layout for that epoch.
    Plan(Vec<u8>),
    /// Member to member on a media link: the receiver cannot decode the sender until its
    /// next keyframe.
    KeyframeRequest,
    /// Member to member on a media link: an app message (a name or chat) sealed with the
    /// sender's frame key for this epoch. At most [`MAX_APP_LEN`] bytes.
    App(Vec<u8>),
    /// Admitted member to host: refreshed direct candidates, over its authenticated link.
    Addresses(Vec<std::net::SocketAddr>),
    /// Host to member: test the signed proposed route before replacing the live route.
    ProbePlan {
        request: u64,
        sealed: Vec<u8>,
    },
    /// Member to host: outcome for exactly that proposed plan.
    PlanChecked {
        request: u64,
        ok: bool,
    },
    /// Existing member reconnects with its pinned identity and last applied MLS epoch.
    Resume {
        room_id: [u8; 16],
        epoch: u64,
    },
    /// Host accepts recovery; commits through this epoch follow before the newest plan.
    Resumed {
        epoch: u64,
    },
    /// Host intentionally ends this call; clients must not attempt recovery.
    Ended,
    // Planned host handover in a persistent call (ADR 0010). New variants go last, so older
    // peers still decode every earlier one.
    /// Available member to host only: a coarse report of this device's own connection.
    Availability(Availability),
    /// Host to the chosen successor: start host roles for the call at `epoch`.
    HandoverOffer { request: u64, epoch: u64 },
    /// Successor to host: `contact` is the successor's signed address set, as an invite would
    /// carry it. `None` declines or reports that host roles could not start.
    HandoverReady { request: u64, contact: Option<Vec<u8>> },
    /// Host to members, after the handover commit for `epoch`: dial the new host at `contact`.
    /// `removed` holds device keys removed or declined earlier in this call, at most
    /// [`MAX_HANDOVER_REMOVED`], so the new host's lobby can flag them.
    HandedOver {
        epoch: u64,
        contact: Vec<u8>,
        removed: Vec<[u8; 32]>,
    },
}

/// How directly a device can be reached, reduced to three values so no address is implied.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reachable {
    /// Reachable without a relay or a punch: a public, mapped or IPv6 address.
    Good,
    /// Reachable after a hole punch.
    Maybe,
    /// Only through a relay.
    No,
}

/// A member's coarse report of its own connection, sent only to the current host. It holds
/// no address, no member list and no per-peer figure (ADR 0010 section 6).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    /// "Available to host" is on and this device can run host roles.
    pub available: bool,
    pub reachable: Reachable,
    /// Members this device has a direct (not relayed) media link to.
    pub direct_links: u8,
    /// Median round trip to those members, in 10 ms steps, at most 100 (1 second).
    pub rtt_10ms: u8,
    /// Recent achieved upload, in 500 kbit/s steps.
    pub upload_500k: u16,
}

impl Control {
    /// Checks the bounds a decoder cannot: list lengths in handover messages.
    pub fn within_bounds(&self) -> bool {
        match self {
            Self::HandoverReady {
                contact: Some(contact),
                ..
            } => contact.len() <= MAX_CONTACT_LEN,
            Self::HandedOver {
                contact, removed, ..
            } => contact.len() <= MAX_CONTACT_LEN && removed.len() <= MAX_HANDOVER_REMOVED,
            Self::Availability(report) => report.rtt_10ms <= 100,
            _ => true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    UnknownRoom,
    TokenUsed,
    DeclinedByHost,
    RoomFull,
    /// The host handed the call to someone else; the invite no longer works.
    HostChanged,
    /// The call needs a newer Veil (for example a persistent call).
    NeedsUpdate,
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

pub async fn write<T: Serialize>(
    stream: &mut quinn::SendStream,
    message: &T,
) -> Result<(), NetError> {
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
    fn earlier_variants_keep_their_encoding() {
        // Older peers decode these by index, so new variants must only be appended.
        assert_eq!(postcard::to_allocvec(&Control::Ended).unwrap(), [17]);
        assert_eq!(postcard::to_allocvec(&RefuseReason::RoomFull).unwrap(), [3]);
    }

    #[test]
    fn handover_bounds() {
        let over = Control::HandedOver {
            epoch: 3,
            contact: vec![0; 8],
            removed: vec![[0; 32]; MAX_HANDOVER_REMOVED + 1],
        };
        assert!(!over.within_bounds());
        let ok = Control::HandedOver {
            epoch: 3,
            contact: vec![0; 8],
            removed: vec![[0; 32]; MAX_HANDOVER_REMOVED],
        };
        assert!(ok.within_bounds());
        let bytes = encode(&ok).unwrap();
        assert_eq!(decode::<Control>(&bytes[4..]).unwrap(), ok);
        let big = Control::HandoverReady {
            request: 1,
            contact: Some(vec![0; MAX_CONTACT_LEN + 1]),
        };
        assert!(!big.within_bounds());
    }

    #[test]
    fn garbage_is_refused() {
        assert!(decode::<Control>(&[0xff, 0xff, 0xff]).is_err());
    }
}
