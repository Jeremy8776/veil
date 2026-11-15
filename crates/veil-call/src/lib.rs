//! Veil call sessions: the participant-side glue between links, the lobby and MLS.
//!
//! Purpose: the host runs [`HostHandle`], which admits joiners from its lobby over their pinned
//! QUIC links and sends every epoch change to the members. A joiner calls [`join`] with an
//! invite and gets a [`MemberHandle`]. Both sides report each new epoch as [`EpochKeys`], the
//! frame sender and receiver the media pipeline uses.
//!
//! Invariants:
//! - Only participants run this crate. Nodes (relays, forwarders, rendezvous) never hold group
//!   state, so they never see these keys.
//! - The host's group and lobby live in one task. Links talk to it through channels, so there is
//!   no lock around MLS state and every change happens in one order.
//! - A link closing is a leave: the host removes that member, which starts a new epoch.
//! - No function here panics. Failures are a [`CallError`].
//!
//! shortcut: the new epoch's keys replace the old ones at once, so frames sealed just before a
//! change are refused by receivers that already moved on. Upgrade by keeping the previous
//! receiver for a short grace period in the media pipeline.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod host;
pub mod member;

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use veil_group::{Group, GroupError};
use veil_net::control::RefuseReason;
use veil_security::PublicId;
use veil_security::frame::{FrameReceiver, FrameSender};

pub use host::{HostEvent, HostHandle};
pub use member::{MemberEvent, MemberHandle, join};

/// Frame keys for one epoch.
pub struct EpochKeys {
    pub epoch: u64,
    /// Seals this member's frames. Its sender index is this member's leaf index.
    pub sender: FrameSender,
    /// Opens every member's frames in this epoch.
    pub receiver: FrameReceiver,
    /// Every member's leaf index and device key in this epoch.
    pub roster: Vec<(u32, PublicId)>,
}

impl EpochKeys {
    pub(crate) fn from_group(group: &Group) -> Result<Self, GroupError> {
        Ok(Self {
            epoch: group.epoch(),
            sender: group.frame_sender()?,
            receiver: group.frame_receiver()?,
            roster: group.members(),
        })
    }
}

impl fmt::Debug for EpochKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochKeys")
            .field("epoch", &self.epoch)
            .field("roster", &self.roster)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum CallError {
    Net(veil_net::NetError),
    Group(GroupError),
    /// The host turned this joiner away.
    Refused(RefuseReason),
    /// The other side sent a message out of turn. The text names what was expected.
    Protocol(&'static str),
    /// No answer in time.
    Timeout,
    /// The session task has stopped.
    Closed,
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net(e) => write!(f, "{e}"),
            Self::Group(e) => write!(f, "{e}"),
            Self::Refused(r) => write!(f, "host refused: {r:?}"),
            Self::Protocol(what) => write!(f, "protocol violation, wanted {what}"),
            Self::Timeout => f.write_str("the host did not answer in time"),
            Self::Closed => f.write_str("call session has stopped"),
        }
    }
}

impl std::error::Error for CallError {}

impl From<veil_net::NetError> for CallError {
    fn from(e: veil_net::NetError) -> Self {
        Self::Net(e)
    }
}

impl From<GroupError> for CallError {
    fn from(e: GroupError) -> Self {
        Self::Group(e)
    }
}

pub(crate) fn now_unix() -> u64 {
    // A clock before 1970 makes every invite look expired, which fails closed.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs())
}
