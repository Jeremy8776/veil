//! Error type shared by every module in this crate.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    /// Input did not have the expected shape. The text names the part that failed.
    Malformed(&'static str),
    /// A ladder or encoder setting is out of range. The text names it.
    BadConfig(&'static str),
    /// A payload would be larger than a sealed frame may be.
    TooLarge,
    /// The platform encoder failed. The text names the step.
    Encoder(&'static str),
    /// A decoder failed. The text names the step.
    Decoder(&'static str),
    /// This machine has no usable platform media component. The text names it.
    Unavailable(&'static str),
    /// The platform camera failed. The text names the step.
    Capture(&'static str),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(part) => write!(f, "malformed media: {part}"),
            Self::BadConfig(part) => write!(f, "bad media setting: {part}"),
            Self::TooLarge => f.write_str("media payload is too large"),
            Self::Encoder(step) => write!(f, "encoder failed: {step}"),
            Self::Decoder(step) => write!(f, "decoder failed: {step}"),
            Self::Unavailable(what) => write!(f, "not available on this machine: {what}"),
            Self::Capture(step) => write!(f, "camera failed: {step}"),
        }
    }
}

impl std::error::Error for MediaError {}
