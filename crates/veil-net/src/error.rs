//! Error type for the networking layer.

use std::fmt;

#[derive(Debug)]
pub enum NetError {
    /// TLS or QUIC configuration could not be built.
    Config(String),
    /// A socket operation failed.
    Io(std::io::Error),
    /// Every address was tried and none connected.
    Connect(String),
    /// No address was given to dial.
    NoAddresses,
    /// The connection did not finish in time.
    Timeout,
    /// The peer presented no usable device key.
    Identity,
    /// The peer sent something that breaks the protocol.
    Protocol(&'static str),
    /// A message or frame is over its size limit.
    TooLarge,
    /// The link is closed.
    Closed(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(e) => write!(f, "configuration failed: {e}"),
            Self::Io(e) => write!(f, "socket error: {e}"),
            Self::Connect(e) => write!(f, "could not connect: {e}"),
            Self::NoAddresses => f.write_str("no address to connect to"),
            Self::Timeout => f.write_str("connection timed out"),
            Self::Identity => f.write_str("peer presented no valid device key"),
            Self::Protocol(what) => write!(f, "protocol violation: {what}"),
            Self::TooLarge => f.write_str("message is too large"),
            Self::Closed(why) => write!(f, "link closed: {why}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
