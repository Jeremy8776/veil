//! Forwarder control messages, on the link's framed control stream.
//!
//! Media does not use these messages. Video frames use `Link::send_frame` (one stream each)
//! and audio uses datagrams, exactly as on a direct link, so a participant's media code does
//! not care whether its peer is another participant or a forwarder.

use serde::{Deserialize, Serialize};
use veil_security::PublicId;

use crate::subscribe::Wanted;

/// Member to forwarder.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    /// Join the room a sealed [`crate::RosterTicket`] names. Must be the first message.
    Join { ticket: Vec<u8> },
    /// A newer sealed ticket for this member's room, sent after each epoch change.
    Ticket(Vec<u8>),
    /// The highest layers this member wants from each named sender. Senders not named keep
    /// their earlier setting, or every layer.
    Subscribe(Vec<(PublicId, Wanted)>),
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
    /// The ticket does not decode or its signature does not match its host.
    BadTicket,
    /// The ticket does not list this device.
    NotListed,
    /// The room has already seen a ticket from a later epoch.
    StaleTicket,
}
