//! The new host's signed contact, carried in `HandoverReady` and `HandedOver` (ADR 0010 §7).
//!
//! Purpose: members must reach the new host after a planned handover. The successor signs,
//! with its own device key, the addresses and relay an invite of its own would carry, bound to
//! the call's room and to the epoch the handover commit starts.
//!
//! Invariants:
//! - A contact opens only under the key the member's group names as host for that epoch, so
//!   the old host cannot point members at anyone the commit did not name.
//! - It holds at most [`MAX_HINTS`] addresses and one relay field, never a member list.
//! - Strict decode: the signature covers every byte, and nothing follows it.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use veil_net::control::MAX_CONTACT_LEN;
use veil_security::{Identity, PublicId};

use super::hints::{MAX_HINTS, checked_candidates};
use crate::CallError;
use crate::relay_field::RelayAddrs;

const DOMAIN: &[u8] = b"veil handover contact v1\0";
const SIGNATURE_LEN: usize = 64;

/// Where members dial the new host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub room_id: [u8; 16],
    /// The epoch the handover commit starts (E+1).
    pub epoch: u64,
    /// The new host's device key. Must match the signer.
    pub host: PublicId,
    /// Direct addresses, as an invite's `addrs`. Empty for a host reached only by relay.
    pub addrs: Vec<SocketAddr>,
    /// The new host's rendezvous and relay node, in the invite relay field format.
    pub relay: Option<String>,
}

impl Contact {
    /// The relay node this contact names, if it parses.
    pub fn relay_addrs(&self) -> Option<RelayAddrs> {
        self.relay.as_deref().and_then(RelayAddrs::parse)
    }

    /// Signs the contact with the successor's own device key.
    pub fn seal(&self, identity: &Identity) -> Result<Vec<u8>, CallError> {
        if self.host != identity.public() {
            return Err(CallError::Protocol("a contact signed by its own host"));
        }
        self.check()?;
        let mut bytes =
            postcard::to_allocvec(self).map_err(|_| CallError::Protocol("a contact"))?;
        let signature = identity.sign(&signed(&bytes));
        bytes.extend_from_slice(&signature);
        if bytes.len() > MAX_CONTACT_LEN {
            return Err(CallError::Protocol("a smaller contact"));
        }
        Ok(bytes)
    }

    /// Opens a contact signed by `host` for this room and epoch.
    pub fn open(
        sealed: &[u8],
        host: &PublicId,
        room_id: [u8; 16],
        epoch: u64,
    ) -> Result<Self, CallError> {
        if sealed.len() > MAX_CONTACT_LEN {
            return Err(CallError::Protocol("a smaller contact"));
        }
        let split = sealed
            .len()
            .checked_sub(SIGNATURE_LEN)
            .filter(|n| *n > 0)
            .ok_or(CallError::Protocol("a signed contact"))?;
        let (encoded, signature) = sealed.split_at(split);
        let signature: [u8; SIGNATURE_LEN] = signature
            .try_into()
            .map_err(|_| CallError::Protocol("a signed contact"))?;
        host.verify(&signed(encoded), &signature)
            .map_err(|_| CallError::Protocol("a contact signed by the new host"))?;
        let (contact, rest): (Self, &[u8]) = postcard::take_from_bytes(encoded)
            .map_err(|_| CallError::Protocol("a signed contact"))?;
        if !rest.is_empty() {
            return Err(CallError::Protocol("a signed contact"));
        }
        contact.check()?;
        if contact.host != *host || contact.room_id != room_id || contact.epoch != epoch {
            return Err(CallError::Protocol("a contact for this call and epoch"));
        }
        Ok(contact)
    }

    fn check(&self) -> Result<(), CallError> {
        if self.addrs.len() > MAX_HINTS
            || (!self.addrs.is_empty() && checked_candidates(self.addrs.clone()).is_none())
        {
            return Err(CallError::Protocol("valid contact addresses"));
        }
        if self.relay.is_some() && self.relay_addrs().is_none() {
            return Err(CallError::Protocol("a valid contact relay"));
        }
        if self.addrs.is_empty() && self.relay.is_none() {
            return Err(CallError::Protocol("a contact with a way in"));
        }
        Ok(())
    }
}

fn signed(encoded: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + encoded.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(encoded);
    message
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn contact(host: &Identity) -> Contact {
        Contact {
            room_id: [5; 16],
            epoch: 7,
            host: host.public(),
            addrs: vec!["81.2.69.160:5000".parse().unwrap()],
            relay: None,
        }
    }

    #[test]
    fn opens_only_for_its_signer_room_and_epoch() {
        let ana = Identity::generate();
        let sealed = contact(&ana).seal(&ana).unwrap();
        assert_eq!(
            Contact::open(&sealed, &ana.public(), [5; 16], 7).unwrap(),
            contact(&ana)
        );
        let other = Identity::generate().public();
        assert!(Contact::open(&sealed, &other, [5; 16], 7).is_err());
        assert!(Contact::open(&sealed, &ana.public(), [6; 16], 7).is_err());
        assert!(Contact::open(&sealed, &ana.public(), [5; 16], 8).is_err());
        let mut tampered = sealed.clone();
        tampered[3] ^= 1;
        assert!(Contact::open(&tampered, &ana.public(), [5; 16], 7).is_err());
    }

    #[test]
    fn nobody_signs_a_contact_for_someone_else() {
        let ana = Identity::generate();
        let bob = Identity::generate();
        assert!(contact(&ana).seal(&bob).is_err());
    }

    #[test]
    fn a_contact_needs_a_way_in_and_bounded_addresses() {
        let ana = Identity::generate();
        let mut empty = contact(&ana);
        empty.addrs.clear();
        assert!(empty.seal(&ana).is_err());
        let mut many = contact(&ana);
        many.addrs = vec!["81.2.69.160:5000".parse().unwrap(); MAX_HINTS + 1];
        assert!(many.seal(&ana).is_err());
        let mut bad_relay = contact(&ana);
        bad_relay.relay = Some("not a relay".into());
        assert!(bad_relay.seal(&ana).is_err());
    }
}
