//! Error type for group state and lobby admission.

use std::fmt;

use veil_net::control::RefuseReason;
use veil_security::SecurityError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupError {
    /// The lobby refused a joiner. Send this reason back as `Control::Refused`.
    Refused(RefuseReason),
    /// Only the host may add or remove members.
    NotHost,
    /// A commit came from someone other than the host.
    NotFromHost,
    /// Only a persistent call can hand its host role to another member.
    NotPersistent,
    /// A key package or welcome is not signed by the device it claims to be.
    WrongDevice,
    /// The device is already in the group.
    AlreadyMember,
    /// The device is not in the group, or was not approved in the lobby.
    NotMember,
    /// This member was removed. It holds no keys for the current epoch.
    Removed,
    /// A message of the wrong kind arrived. The text names what was expected.
    Unexpected(&'static str),
    /// Input did not decode. The text names the part that failed.
    Malformed(&'static str),
    /// openmls refused an operation. The text is openmls's own description.
    Mls(String),
    /// The per-frame encryption layer refused a key or epoch.
    Security(SecurityError),
}

impl GroupError {
    pub(crate) fn mls(error: impl fmt::Display) -> Self {
        Self::Mls(error.to_string())
    }
}

impl From<SecurityError> for GroupError {
    fn from(error: SecurityError) -> Self {
        Self::Security(error)
    }
}

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "joiner refused: {reason:?}"),
            Self::NotHost => f.write_str("only the host can change the group"),
            Self::NotFromHost => f.write_str("commit was not sent by the host"),
            Self::NotPersistent => f.write_str("this call ends with its host"),
            Self::WrongDevice => f.write_str("message is not signed by the expected device"),
            Self::AlreadyMember => f.write_str("device is already a member"),
            Self::NotMember => f.write_str("device is not a member"),
            Self::Removed => f.write_str("this member was removed from the group"),
            Self::Unexpected(what) => write!(f, "unexpected message, wanted {what}"),
            Self::Malformed(part) => write!(f, "malformed input: {part}"),
            Self::Mls(text) => write!(f, "group protocol error: {text}"),
            Self::Security(error) => write!(f, "frame security: {error}"),
        }
    }
}

impl std::error::Error for GroupError {}
