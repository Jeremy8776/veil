//! Error type shared by every module in this crate.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityError {
    /// A signature did not verify.
    BadSignature,
    /// A public or secret key was not valid.
    BadKey,
    /// Input did not have the expected shape. The text names the part that failed.
    Malformed(&'static str),
    /// An invite is past its expiry time.
    Expired,
    /// Input is larger than the limit for its kind.
    TooLarge,
    /// A frame names a key this receiver does not hold (wrong epoch).
    UnknownKey,
    /// A frame counter was already seen, or is too old to check.
    Replay,
    /// Authenticated decryption failed: wrong key or tampered data.
    Decrypt,
    /// Encryption failed inside the cipher.
    Encrypt,
    /// A sender used every counter value for this key. A new epoch is required.
    CounterExhausted,
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSignature => f.write_str("signature did not verify"),
            Self::BadKey => f.write_str("key is not valid"),
            Self::Malformed(part) => write!(f, "malformed input: {part}"),
            Self::Expired => f.write_str("invite has expired"),
            Self::TooLarge => f.write_str("input is too large"),
            Self::UnknownKey => f.write_str("frame uses an unknown key"),
            Self::Replay => f.write_str("frame was replayed or is too old"),
            Self::Decrypt => f.write_str("decryption failed"),
            Self::Encrypt => f.write_str("encryption failed"),
            Self::CounterExhausted => f.write_str("frame counter exhausted, rekey required"),
        }
    }
}

impl std::error::Error for SecurityError {}
