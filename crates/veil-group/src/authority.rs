//! Who may commit: the call's host, named in the MLS group context (ADR 0010).
//!
//! Purpose: a persistent call names its host in a Veil group context extension,
//! [`CallAuthority`], so the host can change by one commit and every member agrees who the
//! host is at every epoch. A call that is not persistent carries no extension and its host
//! stays the creator at leaf 0, exactly as before.
//!
//! Invariants:
//! - Exactly one committer per epoch: the device [`CallAuthority::host`] names, or leaf 0 when
//!   the group has no extension.
//! - A persistent group lists the extension in its required capabilities, so a device whose
//!   key package does not support it (an older Veil) cannot be added.
//! - A handover commit changes only the host, to another current member, and keeps the call
//!   persistent. Members check this before merging, so a bad handover never takes effect.
//! - The old host's private key never moves. The new host commits with its own device key.

use openmls::prelude::{
    Capabilities, Extension, ExtensionType, Extensions, GroupContext,
    RequiredCapabilitiesExtension, UnknownExtension,
};
use veil_security::PublicId;

use crate::GroupError;
use crate::group::{DeviceSigner, Group, check_sendable, encode};
use veil_net::control::Control;

/// Veil's extension type, from the private-use range of RFC 9420 section 17.3.
pub const AUTHORITY_EXTENSION: u16 = 0xF5A1;
const VERSION: u8 = 1;
const ENCODED_LEN: usize = 2 + 32;

/// Who hosts the call now, and whether the call outlives its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallAuthority {
    pub host: PublicId,
    pub persistent: bool,
}

impl CallAuthority {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ENCODED_LEN);
        out.push(VERSION);
        out.push(u8::from(self.persistent));
        out.extend_from_slice(&self.host.0);
        out
    }

    /// Strict: one version, a 0 or 1 flag, a 32-byte key and nothing after it.
    pub fn decode(bytes: &[u8]) -> Result<Self, GroupError> {
        let [version, flag, key @ ..] = bytes else {
            return Err(GroupError::Malformed("authority"));
        };
        if *version != VERSION || key.len() != 32 {
            return Err(GroupError::Malformed("authority"));
        }
        let persistent = match flag {
            0 => false,
            1 => true,
            _ => return Err(GroupError::Malformed("authority")),
        };
        let host = PublicId(
            key.try_into()
                .map_err(|_| GroupError::Malformed("authority"))?,
        );
        Ok(Self { host, persistent })
    }

    /// Reads the extension from a group context. `Ok(None)` means a call that is not persistent.
    pub fn read(extensions: &Extensions<GroupContext>) -> Result<Option<Self>, GroupError> {
        extensions
            .unknown(AUTHORITY_EXTENSION)
            .map(|UnknownExtension(bytes)| Self::decode(bytes))
            .transpose()
    }

    /// The group context extensions that carry this authority.
    pub(crate) fn extensions(&self) -> Result<Extensions<GroupContext>, GroupError> {
        let required = RequiredCapabilitiesExtension::new(&[extension_type()], &[], &[]);
        Extensions::from_vec(vec![
            Extension::RequiredCapabilities(required),
            Extension::Unknown(AUTHORITY_EXTENSION, UnknownExtension(self.encode())),
        ])
        .map_err(GroupError::mls)
    }
}

fn extension_type() -> ExtensionType {
    ExtensionType::Unknown(AUTHORITY_EXTENSION)
}

/// True when a leaf can join a persistent call.
pub(crate) fn supports_authority(capabilities: &Capabilities) -> bool {
    capabilities.extensions().contains(&extension_type())
}

/// Leaf capabilities every Veil device advertises, so it can join a persistent call.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities::new(None, None, Some(&[extension_type()]), None, None)
}

impl Group {
    /// The call's authority, if it is persistent.
    pub fn authority(&self) -> Result<Option<CallAuthority>, GroupError> {
        CallAuthority::read(self.mls.extensions())
    }

    pub fn is_persistent(&self) -> bool {
        matches!(self.authority(), Ok(Some(a)) if a.persistent)
    }

    /// The device allowed to commit in the current epoch.
    pub fn host(&self) -> Option<PublicId> {
        match self.authority() {
            Ok(Some(authority)) => Some(authority.host),
            Ok(None) => self.device_at(crate::group::HOST_LEAF),
            Err(_) => None,
        }
    }

    pub(crate) fn host_leaf(&self) -> Option<u32> {
        self.host().and_then(|host| self.leaf_of(&host))
    }

    /// Host only, persistent calls only: names `successor` as host from the next epoch.
    /// Returns the `Control::Commit` for every other member. From the next epoch this device
    /// is an ordinary member and the successor commits its removal.
    pub fn hand_over(&mut self, successor: &PublicId) -> Result<Vec<u8>, GroupError> {
        if !self.is_host() {
            return Err(GroupError::NotHost);
        }
        let Some(current) = self.authority()? else {
            return Err(GroupError::NotPersistent);
        };
        if !current.persistent {
            return Err(GroupError::NotPersistent);
        }
        if *successor == self.identity.public() || self.leaf_of(successor).is_none() {
            return Err(GroupError::NotMember);
        }
        let next = CallAuthority {
            host: *successor,
            persistent: true,
        };
        let (commit, _, _) = self
            .mls
            .update_group_context_extensions(
                &self.provider,
                next.extensions()?,
                &DeviceSigner(&self.identity),
            )
            .map_err(GroupError::mls)?;
        let built = encode(&commit).and_then(|commit| {
            check_sendable(&Control::Commit(commit.clone()))?;
            Ok(commit)
        });
        self.finish_commit(built)
    }
}

/// Checks the authority a staged commit would move to, before it is merged.
/// `before` is the current authority. Only the host may change, and only to a member.
pub(crate) fn check_transition(
    before: Option<CallAuthority>,
    after: Option<CallAuthority>,
    is_member_after: impl Fn(&PublicId) -> bool,
) -> Result<(), GroupError> {
    match (before, after) {
        (None, None) => Ok(()),
        (Some(before), Some(after)) => {
            if after.persistent != before.persistent || !is_member_after(&after.host) {
                return Err(GroupError::Malformed("authority change"));
            }
            Ok(())
        }
        // A call never becomes persistent, or stops being so, part way through.
        _ => Err(GroupError::Malformed("authority change")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicId {
        PublicId([byte; 32])
    }

    #[test]
    fn round_trips_and_refuses_malformed() {
        let a = CallAuthority {
            host: key(7),
            persistent: true,
        };
        assert_eq!(CallAuthority::decode(&a.encode()).unwrap(), a);
        let mut bad = a.encode();
        bad[1] = 2;
        assert!(CallAuthority::decode(&bad).is_err());
        let mut long = a.encode();
        long.push(0);
        assert!(CallAuthority::decode(&long).is_err());
        assert!(CallAuthority::decode(&a.encode()[..10]).is_err());
        let mut version = a.encode();
        version[0] = 9;
        assert!(CallAuthority::decode(&version).is_err());
    }

    #[test]
    fn transition_rules() {
        let persistent = |host| {
            Some(CallAuthority {
                host,
                persistent: true,
            })
        };
        let members = |d: &PublicId| *d == key(1) || *d == key(2);
        assert!(check_transition(None, None, members).is_ok());
        assert!(check_transition(persistent(key(1)), persistent(key(2)), members).is_ok());
        assert!(check_transition(persistent(key(1)), persistent(key(9)), members).is_err());
        assert!(check_transition(None, persistent(key(1)), members).is_err());
        assert!(check_transition(persistent(key(1)), None, members).is_err());
        let stops = Some(CallAuthority {
            host: key(2),
            persistent: false,
        });
        assert!(check_transition(persistent(key(1)), stops, members).is_err());
    }
}
