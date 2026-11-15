//! Forwarder control messages, on the link's framed control stream.
//!
//! Media does not use these messages. Video frames use `Link::send_frame` (one stream each)
//! and audio uses datagrams, exactly as on a direct link, so a participant's media code does
//! not care whether its peer is another participant or a forwarder.

use serde::{Deserialize, Serialize};

/// Member to forwarder.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    /// Join a room, named by its tag. Must be the first message.
    Join { room_tag: [u8; 32] },
}

/// Forwarder to member.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    /// Joined. `members` counts everyone in the room, including this member.
    Joined { members: u16 },
    Refused(Refusal),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// This device is already in the room on another link.
    AlreadyJoined,
    /// The room has its largest allowed number of members.
    RoomFull,
    /// The forwarder has its largest allowed number of rooms.
    Full,
}
