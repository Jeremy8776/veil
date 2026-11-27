//! One participant's view of a call's MLS group.
//!
//! Purpose: hold the MLS state, turn joins and leaves into new epochs, and hand each epoch's
//! 32-byte secret to `veil-security`'s per-frame encryption.
//!
//! Invariants:
//! - The host is the only committer. Other members refuse commits from anyone else, so a
//!   member cannot add or remove people. The host is the creator at leaf 0, unless the call
//!   is persistent: then the group context names it and it can change (`authority.rs`).
//! - Every add and remove is one commit, so every join or leave starts a new epoch.
//! - Each member's MLS signature key is its Veil device key. A key package is accepted only when
//!   it is signed by the device key the QUIC link was pinned to.
//! - A member's frame sender index is its MLS leaf index; the frame key id also carries the
//!   epoch, so a reused leaf slot never reuses a key.
//!
//! shortcut: the host is the only committer, so the group cannot change while the host is
//! away, and an unplanned host loss still ends the call. Planned handover is in
//! `authority.rs`; upgrade with an election design (HOST-02) for unplanned loss.

use openmls::prelude::{
    BasicCredential, Credential, CredentialWithKey, GroupId, KeyPackage, KeyPackageIn,
    LeafNodeIndex, MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, MlsMessageBodyIn,
    MlsMessageIn, MlsMessageOut, PURE_CIPHERTEXT_WIRE_FORMAT_POLICY, ProcessedMessageContent,
    ProtocolMessage, ProtocolVersion, Sender, StagedWelcome,
};
use openmls_traits::OpenMlsProvider;
use openmls_traits::crypto::OpenMlsCrypto as _;
use openmls_traits::signatures::{Signer, SignerError};
use openmls_traits::types::SignatureScheme;
use tls_codec::{Deserialize as _, Serialize as _, VLBytes};
use veil_net::control::{self, Control};
use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::{Identity, PublicId};
use zeroize::Zeroizing;

use crate::GroupError;
use crate::authority::{self, CallAuthority};
use crate::provider::{CIPHERSUITE, VeilProvider};

/// Most members in one call, matching the agreed scope of about 16 people.
pub const MAX_MEMBERS: usize = 16;
/// Longest key package the host reads. An honest one is about 300 bytes. With every member of a
/// full call at this size, the largest Welcome is about 30 KiB, under half the control limit.
pub const MAX_KEY_PACKAGE_LEN: usize = 2_048;
/// Exporter label for the media epoch secret.
const EXPORTER_LABEL: &str = "veil-media-v1";
pub(crate) const HOST_LEAF: u32 = 0;

/// Signs MLS messages with the Veil device key.
pub(crate) struct DeviceSigner<'a>(pub(crate) &'a Identity);

impl Signer for DeviceSigner<'_> {
    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
        Ok(self.0.sign(payload).to_vec())
    }

    fn signature_scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}

fn credential(identity: &Identity) -> CredentialWithKey {
    let public = identity.public().0.to_vec();
    CredentialWithKey {
        credential: Credential::from(BasicCredential::new(public.clone())),
        signature_key: public.into(),
    }
}

fn copy_identity(identity: &Identity) -> Identity {
    Identity::from_secret(&identity.secret_bytes())
}

fn create_config(authority: Option<CallAuthority>) -> Result<MlsGroupCreateConfig, GroupError> {
    let builder = MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .capabilities(authority::capabilities());
    Ok(match authority {
        Some(authority) => builder.with_group_context_extensions(authority.extensions()?),
        None => builder,
    }
    .build())
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .wire_format_policy(PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
        .use_ratchet_tree_extension(true)
        .build()
}

pub(crate) fn encode(message: &MlsMessageOut) -> Result<Vec<u8>, GroupError> {
    message
        .tls_serialize_detached()
        .map_err(|_| GroupError::Malformed("encode"))
}

fn decode(bytes: &[u8]) -> Result<MlsMessageBodyIn, GroupError> {
    let mut reader = bytes;
    let message = MlsMessageIn::tls_deserialize(&mut reader)
        .map_err(|_| GroupError::Malformed("mls message"))?;
    if !reader.is_empty() {
        return Err(GroupError::Malformed("trailing bytes"));
    }
    Ok(message.extract())
}

/// Refuses a message that could not be sent on a control stream.
pub(crate) fn check_sendable(message: &Control) -> Result<(), GroupError> {
    control::encode(message)
        .map(drop)
        .map_err(|_| GroupError::Malformed("message too large to send"))
}

/// Key ids are bounded to 32-byte Ed25519 keys before comparison.
fn device_of(signature_key: &[u8]) -> Option<PublicId> {
    signature_key.try_into().ok().map(PublicId)
}

/// A device that has an invite and is waiting for the host's Welcome.
pub struct Joiner {
    provider: VeilProvider,
    identity: Identity,
}

impl Joiner {
    pub fn new(identity: &Identity) -> Result<Self, GroupError> {
        Ok(Self {
            provider: VeilProvider::new().map_err(GroupError::mls)?,
            identity: copy_identity(identity),
        })
    }

    /// A fresh key package to send as `Control::KeyPackage`. Its private keys stay here.
    pub fn key_package(&self) -> Result<Vec<u8>, GroupError> {
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(authority::capabilities())
            .build(
                CIPHERSUITE,
                &self.provider,
                &DeviceSigner(&self.identity),
                credential(&self.identity),
            )
            .map_err(GroupError::mls)?;
        bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|_| GroupError::Malformed("encode"))
    }

    /// Joins from the host's Welcome. `host` is the device key named in the invite.
    pub fn join(self, welcome: &[u8], host: &PublicId) -> Result<Group, GroupError> {
        let MlsMessageBodyIn::Welcome(welcome) = decode(welcome)? else {
            return Err(GroupError::Unexpected("welcome"));
        };
        let mls = StagedWelcome::new_from_welcome(&self.provider, &join_config(), welcome, None)
            .map_err(GroupError::mls)?
            .into_group(&self.provider)
            .map_err(GroupError::mls)?;
        let group = Group {
            provider: self.provider,
            identity: self.identity,
            mls,
        };
        if group.host() != Some(*host) {
            return Err(GroupError::WrongDevice);
        }
        Ok(group)
    }
}

/// What the host sends after admitting someone.
pub struct Admission {
    /// `Control::Commit` for every existing member except the host.
    pub commit: Vec<u8>,
    /// `Control::Welcome` for the new member.
    pub welcome: Vec<u8>,
}

/// The result of applying the host's commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// A new epoch started. Rebuild frame senders and receivers.
    NewEpoch(u64),
    /// This member was removed and holds no keys from now on.
    Removed,
}

pub struct Group {
    pub(crate) provider: VeilProvider,
    pub(crate) identity: Identity,
    pub(crate) mls: MlsGroup,
}

impl Group {
    /// Creates a call's group. The creator is the host, at leaf 0.
    pub fn create(identity: &Identity, room_id: [u8; 16]) -> Result<Self, GroupError> {
        Self::create_with(identity, room_id, None)
    }

    /// Creates a persistent call's group: the call can outlive the creator through
    /// [`Group::hand_over`]. Only devices that support the authority extension can join.
    pub fn create_persistent(identity: &Identity, room_id: [u8; 16]) -> Result<Self, GroupError> {
        let authority = CallAuthority {
            host: identity.public(),
            persistent: true,
        };
        Self::create_with(identity, room_id, Some(authority))
    }

    fn create_with(
        identity: &Identity,
        room_id: [u8; 16],
        authority: Option<CallAuthority>,
    ) -> Result<Self, GroupError> {
        let provider = VeilProvider::new().map_err(GroupError::mls)?;
        let mls = MlsGroup::new_with_group_id(
            &provider,
            &DeviceSigner(identity),
            &create_config(authority)?,
            GroupId::from_slice(&room_id),
            credential(identity),
        )
        .map_err(GroupError::mls)?;
        Ok(Self {
            provider,
            identity: copy_identity(identity),
            mls,
        })
    }

    pub fn epoch(&self) -> u64 {
        self.mls.epoch().as_u64()
    }

    /// This member's leaf index, which is also its frame sender index.
    pub fn leaf_index(&self) -> u32 {
        self.mls.own_leaf_index().u32()
    }

    /// Whether this device may commit in the current epoch.
    pub fn is_host(&self) -> bool {
        self.host() == Some(self.identity.public())
    }

    pub fn is_active(&self) -> bool {
        self.mls.is_active()
    }

    /// Every member's leaf index and device key.
    pub fn members(&self) -> Vec<(u32, PublicId)> {
        self.mls
            .members()
            .filter_map(|m| device_of(&m.signature_key).map(|d| (m.index.u32(), d)))
            .collect()
    }

    pub(crate) fn device_at(&self, leaf: u32) -> Option<PublicId> {
        self.members()
            .into_iter()
            .find_map(|(index, device)| (index == leaf).then_some(device))
    }

    pub(crate) fn leaf_of(&self, device: &PublicId) -> Option<u32> {
        self.members()
            .into_iter()
            .find_map(|(index, d)| (d == *device).then_some(index))
    }

    /// Refuses a public key the host cannot encrypt to, using the operation commits depend on.
    fn check_can_encrypt_to(&self, public_key: &[u8]) -> Result<(), GroupError> {
        self.provider
            .crypto()
            .hpke_seal(
                CIPHERSUITE.hpke_config(),
                public_key,
                b"veil-key-check",
                b"",
                b"",
            )
            .map(drop)
            .map_err(|_| GroupError::Malformed("unusable public key"))
    }

    /// Moves to the pending commit's epoch only if `built` holds messages that can be sent.
    /// Otherwise the commit is dropped and the group stays in its current epoch.
    pub(crate) fn finish_commit<T>(
        &mut self,
        built: Result<T, GroupError>,
    ) -> Result<T, GroupError> {
        match built {
            Ok(messages) => {
                self.mls
                    .merge_pending_commit(&self.provider)
                    .map_err(GroupError::mls)?;
                Ok(messages)
            }
            Err(error) => {
                self.mls
                    .clear_pending_commit(self.provider.storage())
                    .map_err(GroupError::mls)?;
                Err(error)
            }
        }
    }

    /// Host only: adds the joiner whose QUIC link is pinned to `device`.
    pub fn add(&mut self, key_package: &[u8], device: &PublicId) -> Result<Admission, GroupError> {
        if !self.is_host() {
            return Err(GroupError::NotHost);
        }
        if self.members().len() >= MAX_MEMBERS {
            return Err(GroupError::Refused(
                veil_net::control::RefuseReason::RoomFull,
            ));
        }
        if self.leaf_of(device).is_some() {
            return Err(GroupError::AlreadyMember);
        }
        if key_package.len() > MAX_KEY_PACKAGE_LEN {
            return Err(GroupError::Malformed("key package too long"));
        }
        let mut reader = key_package;
        let package = KeyPackageIn::tls_deserialize(&mut reader)
            .map_err(|_| GroupError::Malformed("key package"))?;
        if !reader.is_empty() {
            return Err(GroupError::Malformed("trailing bytes"));
        }
        let package = package
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(GroupError::mls)?;
        if package.ciphersuite() != CIPHERSUITE {
            return Err(GroupError::Malformed("ciphersuite"));
        }
        let leaf = package.leaf_node();
        let identity = BasicCredential::try_from(leaf.credential().clone())
            .map_err(|_| GroupError::Malformed("credential"))?;
        if leaf.signature_key().as_slice() != device.0 || identity.identity() != device.0 {
            return Err(GroupError::WrongDevice);
        }
        // Every later commit encrypts to this leaf, so one unusable key would block them all.
        let encryption_key = leaf
            .encryption_key()
            .tls_serialize_detached()
            .map_err(|_| GroupError::Malformed("encode"))?;
        let encryption_key = VLBytes::tls_deserialize(&mut encryption_key.as_slice())
            .map_err(|_| GroupError::Malformed("encryption key"))?;
        self.check_can_encrypt_to(encryption_key.as_slice())?;
        self.check_can_encrypt_to(package.hpke_init_key().as_slice())?;
        let (commit, welcome, _) = self
            .mls
            .add_members(&self.provider, &DeviceSigner(&self.identity), &[package])
            .map_err(GroupError::mls)?;
        let built = encode(&commit).and_then(|commit| {
            let welcome = encode(&welcome)?;
            check_sendable(&Control::Commit(commit.clone()))?;
            check_sendable(&Control::Welcome(welcome.clone()))?;
            Ok(Admission { commit, welcome })
        });
        self.finish_commit(built)
    }

    /// Host only: removes a member. Returns the `Control::Commit` for everyone, including the
    /// removed member, who learns from it that it was removed.
    pub fn remove(&mut self, device: &PublicId) -> Result<Vec<u8>, GroupError> {
        if !self.is_host() {
            return Err(GroupError::NotHost);
        }
        let leaf = self.leaf_of(device).ok_or(GroupError::NotMember)?;
        // MLS does not let a committer remove itself; a leaving host hands over instead.
        if leaf == self.leaf_index() {
            return Err(GroupError::NotHost);
        }
        let (commit, _, _) = self
            .mls
            .remove_members(
                &self.provider,
                &DeviceSigner(&self.identity),
                &[LeafNodeIndex::new(leaf)],
            )
            .map_err(GroupError::mls)?;
        let built = encode(&commit).and_then(|commit| {
            check_sendable(&Control::Commit(commit.clone()))?;
            Ok(commit)
        });
        self.finish_commit(built)
    }

    /// Applies a commit received as `Control::Commit`. Refuses commits not sent by the current
    /// epoch's host, and a handover that would name a non-member or change persistence.
    pub fn apply(&mut self, commit: &[u8]) -> Result<Applied, GroupError> {
        if !self.is_active() {
            return Err(GroupError::Removed);
        }
        let message: ProtocolMessage = match decode(commit)? {
            MlsMessageBodyIn::PrivateMessage(m) => m.into(),
            MlsMessageBodyIn::PublicMessage(m) => m.into(),
            _ => return Err(GroupError::Unexpected("commit")),
        };
        let processed = self
            .mls
            .process_message(&self.provider, message)
            .map_err(GroupError::mls)?;
        let host_leaf = self.host_leaf().ok_or(GroupError::NotFromHost)?;
        if *processed.sender() != Sender::Member(LeafNodeIndex::new(host_leaf)) {
            return Err(GroupError::NotFromHost);
        }
        let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() else {
            return Err(GroupError::Unexpected("commit"));
        };
        let after = CallAuthority::read(staged.group_context().extensions())?;
        let removed: Vec<u32> = staged
            .remove_proposals()
            .map(|p| p.remove_proposal().removed().u32())
            .collect();
        authority::check_transition(self.authority()?, after, |device| {
            self.leaf_of(device)
                .is_some_and(|leaf| !removed.contains(&leaf))
        })?;
        let removed = staged.self_removed();
        self.mls
            .merge_staged_commit(&self.provider, *staged)
            .map_err(GroupError::mls)?;
        if removed || !self.is_active() {
            return Ok(Applied::Removed);
        }
        Ok(Applied::NewEpoch(self.epoch()))
    }

    /// The current epoch's media secret, from the MLS exporter.
    pub fn epoch_secret(&self) -> Result<Zeroizing<[u8; 32]>, GroupError> {
        if !self.is_active() {
            return Err(GroupError::Removed);
        }
        let exported = Zeroizing::new(
            self.mls
                .export_secret(self.provider.crypto(), EXPORTER_LABEL, &[], 32)
                .map_err(GroupError::mls)?,
        );
        let mut secret = Zeroizing::new([0u8; 32]);
        if exported.len() != secret.len() {
            return Err(GroupError::Malformed("exporter length"));
        }
        secret.copy_from_slice(&exported);
        Ok(secret)
    }

    /// Seals this member's frames for the current epoch.
    pub fn frame_sender(&self) -> Result<FrameSender, GroupError> {
        Ok(FrameSender::new(
            &*self.epoch_secret()?,
            self.epoch(),
            self.leaf_index(),
        )?)
    }

    /// Opens every member's frames for the current epoch.
    pub fn frame_receiver(&self) -> Result<FrameReceiver, GroupError> {
        Ok(FrameReceiver::new(&*self.epoch_secret()?, self.epoch())?)
    }
}
