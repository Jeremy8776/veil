//! Rendezvous and relay: how two Veil nodes find each other without a cloud service.
//!
//! Any node can run [`RendezvousServer`]: a participant with a public address, a box you run
//! yourself, or an enterprise node. It does three jobs:
//!
//! 1. **Reflect.** Tells each node the public address it appears from, so invites and peers
//!    can use it.
//! 2. **Introduce.** Sends two members of the same room each other's addresses, timed so both
//!    dial at the same moment. That simultaneous dial is what opens most home routers (hole
//!    punching).
//! 3. **Relay.** When direct connection fails, forwards packets between the two. The packets
//!    are the two nodes' own end-to-end QUIC link, pinned to each other's device keys, so
//!    the relay carries ciphertext it cannot read or alter.
//!
//! Invariants:
//! - The server never learns room ids. Members register under [`room_tag`], a hash of the
//!   room id, which only invite holders can compute.
//! - Introductions and relay sessions only join two keys registered in the same room, and
//!   only once the asked member accepts. Until then the asker learns nothing about it.
//! - A relay session forwards only between its two members. Anyone else's packets are dropped.
//! - A dishonest server can deny service and see metadata (who, when, how much). It cannot
//!   impersonate a peer: every dial names the key it expects.
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
pub mod socket;

use std::fmt;

pub use client::{Punch, RendezvousClient, punch};
pub use protocol::{Ask, Refusal, room_tag};
pub use server::RendezvousServer;
pub use socket::RelaySocket;

#[derive(Debug)]
pub enum RendezvousError {
    Net(veil_net::NetError),
    /// The server refused the request.
    Refused(Refusal),
    /// No answer in time.
    Timeout,
    /// The link to the server closed.
    Closed,
}

impl fmt::Display for RendezvousError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net(e) => write!(f, "{e}"),
            Self::Refused(r) => write!(f, "rendezvous refused: {r:?}"),
            Self::Timeout => f.write_str("rendezvous did not answer in time"),
            Self::Closed => f.write_str("rendezvous link closed"),
        }
    }
}

impl std::error::Error for RendezvousError {}

impl From<veil_net::NetError> for RendezvousError {
    fn from(e: veil_net::NetError) -> Self {
        Self::Net(e)
    }
}
