//! The forwarder role: receive each member's stream once, send it to everyone else in the room.
//!
//! Any node can run [`ForwarderServer`]: a participant with good upload, a self-hosted box or an
//! enterprise cluster. It runs the same code everywhere.
//!
//! Invariants:
//! - A forwarder never holds media keys. Video frames and audio packets arrive already sealed
//!   by `veil-security` and leave byte for byte unchanged. It cannot read, forge or relabel them.
//! - Rooms are named by a hash of the room id (`veil_rendezvous::room_tag`), never the id, and
//!   only devices on the host's signed [`RosterTicket`] may join one.
//! - A frame goes to every other member of the sender's room that subscribes to its layer, and
//!   never back to the sender or to another room.
//! - Memory per receiver is bounded in bytes. A slow receiver loses frames; it never stalls the
//!   sender or the other receivers.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod client;
pub mod protocol;
pub mod server;
pub mod subscribe;
pub mod ticket;

use std::fmt;

pub use client::ForwarderClient;
pub use protocol::Refusal;
pub use server::{ForwarderServer, Limits};
pub use subscribe::Wanted;
pub use ticket::{RosterTicket, TicketError};

#[derive(Debug)]
pub enum ForwardError {
    Net(veil_net::NetError),
    /// The forwarder refused to add this member.
    Refused(Refusal),
    /// No answer in time.
    Timeout,
    /// The link closed or the forwarder answered out of turn.
    Closed,
}

impl fmt::Display for ForwardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net(e) => write!(f, "{e}"),
            Self::Refused(r) => write!(f, "forwarder refused: {r:?}"),
            Self::Timeout => f.write_str("forwarder did not answer in time"),
            Self::Closed => f.write_str("forwarder link closed"),
        }
    }
}

impl std::error::Error for ForwardError {}

impl From<veil_net::NetError> for ForwardError {
    fn from(e: veil_net::NetError) -> Self {
        Self::Net(e)
    }
}
