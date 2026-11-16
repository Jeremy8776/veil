//! Veil networking layer: QUIC links between nodes, authenticated by device key.
//!
//! Purpose: connect any two Veil nodes (participant, self-hosted node or enterprise node) over
//! QUIC, prove each side's identity in the TLS 1.3 handshake, carry control messages on a
//! reliable stream, video frames on one short stream each, and audio on datagrams.
//!
//! Invariants:
//! - TLS 1.3 only, with RFC 7250 raw public keys. The device's Ed25519 key *is* the TLS
//!   identity. There is no certificate authority to trust or compromise.
//! - A dialler always names the key it expects. A link to any other key fails the handshake.
//! - A listener learns the dialler's key from the handshake. Whether that key may join a room
//!   is decided above this layer (lobby and group membership), never here.
//! - Media is already sealed by `veil-security` before it reaches this crate. Transport
//!   encryption protects metadata on each hop; it is not what keeps media private.
//! - Every length read from the network is bounded before allocation.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod accept;
pub mod control;
pub mod endpoint;
pub mod error;
pub mod node_addr;
pub mod tls;

pub use control::Control;
pub use endpoint::{FramedRecv, FramedSend, Link, Node};
pub use error::NetError;
pub use node_addr::NodeAddr;
