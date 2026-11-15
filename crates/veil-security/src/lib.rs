//! Veil security core.
//!
//! Purpose: device identity, safety numbers, signed invite links and per-frame media
//! encryption. Group key agreement (MLS) lives in a separate crate and hands this crate one
//! 32-byte epoch secret per epoch.
//!
//! Invariants:
//! - No function here panics. Every failure is a [`SecurityError`].
//! - Raw key bytes live in `Zeroizing` buffers and are wiped on drop.
//! - A (key, nonce) pair is never reused: every sender has its own key id per epoch, and a
//!   sender's frame counter refuses to wrap.
//! - Inputs from the network are bounded before they are parsed.
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
pub mod frame;
pub mod identity;
pub mod invite;
pub mod layer;
pub mod safety;

pub use error::SecurityError;
pub use identity::{Identity, PublicId};
pub use layer::Layer;
