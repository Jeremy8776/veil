//! Veil group keys.
//!
//! Purpose: MLS (RFC 9420, through `openmls`) group state for a call, lobby admission for
//! joiners with a one-time invite token, and the bridge from each MLS epoch to
//! `veil-security`'s per-frame encryption.
//!
//! Invariants:
//! - No function here panics. Every failure is a [`GroupError`].
//! - Every join or leave is a commit and so a new epoch. A removed member holds no secret for
//!   any later epoch.
//! - Relays and forwarders never see these keys: Welcome and commit messages are encrypted to
//!   members, and the media secret never leaves this crate except as a frame key.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod error;
pub mod group;
pub mod lobby;
pub mod provider;

pub use error::GroupError;
pub use group::{Admission, Applied, Group, Joiner, MAX_MEMBERS};
pub use lobby::{Knock, Lobby};
