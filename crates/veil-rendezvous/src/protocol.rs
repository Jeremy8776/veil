//! Rendezvous messages and relay packet framing.
//!
//! Messages travel on the link's framed control stream. Relayed packets travel as datagrams
//! of `session (u32, big endian) || packet`, or, when a packet is too big for a datagram, on
//! a one-packet stream of `session (u32) || length (u16) || packet`.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use veil_security::PublicId;

pub const MAX_ADDRS: usize = 8;
/// Most lobby passes a host may have open in its room at once.
pub const MAX_PASSES: usize = 64;
pub const SESSION_HEADER: usize = 4;
pub const STREAM_HEADER: usize = 6;

/// The name a room is registered under. A hash, so the server never sees the room id.
pub fn room_tag(room_id: &[u8; 16]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"veil-room-tag-v1")
        .chain_update(room_id)
        .finalize()
        .into()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    /// Join a room's directory with the addresses this node can also be dialled at (its
    /// public IPv6 address, from a dual-stack socket; see `veil_net::family`). Introductions
    /// pass them on after the address the server sees, and dialling tries IPv6 first. A room is
    /// named by its tag and its host's key. The host registers with its own key and no pass;
    /// anyone else must be on the host's allow list, or show a lobby pass the host opened.
    Register {
        room_tag: [u8; 32],
        host: PublicId,
        pass: Option<[u8; 32]>,
        local_addrs: Vec<SocketAddr>,
    },
    /// Host only: who may be in the room. `members` may register at any time; each of
    /// `passes` (see `veil_security::invite::lobby_pass`) lets one key register while it stays
    /// open. Replaces the previous lists, and anyone they no longer cover is dropped.
    Allow {
        members: Vec<PublicId>,
        passes: Vec<[u8; 32]>,
    },
    /// Ask to be introduced to another member for a simultaneous dial. The peer is asked
    /// first ([`ServerMsg::Request`]); nobody's address moves until it accepts.
    Introduce { peer: PublicId },
    /// Ask for a relay session with another member. The peer is asked first.
    OpenRelay { peer: PublicId },
    /// Agree to a [`ServerMsg::Request`] from `from`. A stale or unknown one is ignored.
    Accept { from: PublicId, kind: Ask },
    /// Dial this node's own public IP at `port`, from a socket of the server's other than
    /// this link's, and say whether it answered ([`ServerMsg::Reach`]). Needs no room. Send it
    /// on a link of its own: a server older than this message closes the link it arrives on.
    CheckReach { port: u16 },
    /// Host only: keep this sealed invite for a short link while the room `room_tag`, which
    /// this key hosts, is open ([`crate::invites`]). Send it on a link of its own, as for
    /// `CheckReach`.
    PutInvite {
        room_tag: [u8; 32],
        id: [u8; 32],
        sealed: Vec<u8>,
    },
    /// Asks for the sealed invite stored under `id`. Needs no room.
    GetInvite { id: [u8; 32] },
}

/// What a member asks of another.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ask {
    Introduce,
    Relay,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    /// The address this node's packets arrive from, as the server sees it.
    Registered { observed: SocketAddr },
    /// Dial `peer` at `addrs` after `delay_ms`. Both members receive one.
    Punch {
        peer: PublicId,
        addrs: Vec<SocketAddr>,
        delay_ms: u32,
    },
    /// A relay session with `peer` is open. Both members receive one.
    RelayReady { session: u32, peer: PublicId },
    /// `from` asks for an introduction or a relay session. Answer with
    /// [`ClientMsg::Accept`], or not at all to refuse.
    Request { from: PublicId, kind: Ask },
    Refused(Refusal),
    /// The host's `Allow` took effect.
    Allowed,
    /// The answer to [`ClientMsg::CheckReach`]: the address dialled (this node's reflected
    /// IP and the port it named), and whether a handshake with this node's key completed.
    Reach {
        addr: SocketAddr,
        open: bool,
    },
    /// The host's `PutInvite` took effect.
    InviteStored,
    /// The answer to [`ClientMsg::GetInvite`]; `None` if nothing is stored under that id.
    SealedInvite { sealed: Option<Vec<u8>> },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotRegistered,
    AlreadyRegistered,
    /// Sent only for a request naming the asker itself. A key that is absent gets no answer,
    /// like a member that declines, so the server never says who is online.
    PeerNotInRoom,
    TooManyAddrs,
    /// A server limit was reached (rooms, members, sessions or open requests).
    Full,
    /// This node runs rendezvous without the relay role.
    RelayDisabled,
    /// The host's allow list does not cover this key, and it showed no open lobby pass.
    NotAllowed,
    /// The room's host is not registered, or this node serves another call only.
    UnknownRoom,
    /// A reachability check named port 0.
    BadPort,
}

pub fn session_packet(session: u32, packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SESSION_HEADER + packet.len());
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(packet);
    out
}

pub fn split_session(datagram: &[u8]) -> Option<(u32, &[u8])> {
    let (head, packet) = datagram.split_at_checked(SESSION_HEADER)?;
    let session = u32::from_be_bytes(head.try_into().ok()?);
    Some((session, packet))
}

/// Header for the stream path. `None` if the packet is larger than a u16 length allows.
pub fn stream_header(session: u32, len: usize) -> Option<[u8; STREAM_HEADER]> {
    let len = u16::try_from(len).ok()?;
    let mut h = [0u8; STREAM_HEADER];
    h[..4].copy_from_slice(&session.to_be_bytes());
    h[4..].copy_from_slice(&len.to_be_bytes());
    Some(h)
}

pub fn parse_stream_header(h: [u8; STREAM_HEADER]) -> (u32, usize) {
    (
        u32::from_be_bytes([h[0], h[1], h[2], h[3]]),
        usize::from(u16::from_be_bytes([h[4], h[5]])),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn room_tag_hides_the_room_id_and_is_stable() {
        let id = [3u8; 16];
        assert_eq!(room_tag(&id), room_tag(&id));
        assert_ne!(room_tag(&id), room_tag(&[4u8; 16]));
        assert!(!room_tag(&id).windows(16).any(|w| w == id));
    }

    // Known answer: the rendezvous node and every client must agree on the tag, whatever
    // sha2 version each was built with.
    #[test]
    fn room_tag_bytes_are_fixed() {
        let hex: String = room_tag(&[3u8; 16]).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "8820bad367e1d4a13088aa8e4acb60fde4eb43413e63adf97e0015cae80d8bf8");
    }

    #[test]
    fn session_framing_round_trip() {
        let packet = session_packet(77, b"quic bytes");
        assert_eq!(split_session(&packet), Some((77, &b"quic bytes"[..])));
        assert_eq!(split_session(&[1, 2]), None);
    }

    #[test]
    fn stream_header_round_trip_and_bound() {
        let h = stream_header(9, 1_500).unwrap();
        assert_eq!(parse_stream_header(h), (9, 1_500));
        assert!(stream_header(9, 70_000).is_none());
    }
}
