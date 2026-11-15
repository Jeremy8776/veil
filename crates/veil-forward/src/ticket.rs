//! Roster tickets: the host's signed list of who may join a forwarder room.
//!
//! Purpose: a forwarder holds no group state, so it cannot tell a member from a stranger who
//! learned the room tag (for example a joiner the host declined). The host signs a
//! [`RosterTicket`] for each epoch it forwards: room tag, its own key, the epoch and every
//! member's device key. A member shows it on `Join`; the forwarder admits only listed devices.
//!
//! Invariants:
//! - A forwarder room is keyed by (room tag, host key). Anyone can sign a ticket, but a ticket
//!   from another key opens another room, so it never reaches the real members' frames.
//! - Within a room, epochs only move forward. A newer ticket replaces the roster and the
//!   forwarder drops every member it no longer lists; an older one is refused.
//! - The signature covers a domain tag and the encoded ticket. Decoding is bounded by
//!   [`MAX_TICKET_LEN`] and [`MAX_ROSTER`].

use serde::{Deserialize, Serialize};
use veil_security::{Identity, PublicId};

/// Most devices one ticket may list.
pub const MAX_ROSTER: usize = 256;
/// Most bytes a sealed ticket may take: the roster, the header and the signature.
pub const MAX_TICKET_LEN: usize = 16 * 1024;
const SIGNATURE_LEN: usize = 64;
const DOMAIN: &[u8] = b"veil-forward-ticket-v1";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RosterTicket {
    pub room_tag: [u8; 32],
    /// The host that signed this ticket.
    pub host: PublicId,
    pub epoch: u64,
    /// Every device that may join this room in this epoch.
    pub members: Vec<PublicId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketError {
    TooLarge,
    Malformed,
    BadSignature,
}

impl RosterTicket {
    /// Signs the ticket. `host` must be the identity the ticket names.
    pub fn seal(&self, host: &Identity) -> Result<Vec<u8>, TicketError> {
        if host.public() != self.host {
            return Err(TicketError::BadSignature);
        }
        if self.members.len() > MAX_ROSTER {
            return Err(TicketError::TooLarge);
        }
        let mut bytes = postcard::to_allocvec(self).map_err(|_| TicketError::Malformed)?;
        let signature = host.sign(&signed_message(&bytes));
        bytes.extend_from_slice(&signature);
        if bytes.len() > MAX_TICKET_LEN {
            return Err(TicketError::TooLarge);
        }
        Ok(bytes)
    }

    /// Decodes a sealed ticket and checks it is signed by the host it names.
    pub fn open(sealed: &[u8]) -> Result<Self, TicketError> {
        if sealed.len() > MAX_TICKET_LEN {
            return Err(TicketError::TooLarge);
        }
        let split = sealed
            .len()
            .checked_sub(SIGNATURE_LEN)
            .filter(|n| *n > 0)
            .ok_or(TicketError::Malformed)?;
        let (encoded, signature) = sealed.split_at(split);
        let signature: [u8; SIGNATURE_LEN] =
            signature.try_into().map_err(|_| TicketError::Malformed)?;
        let ticket: Self = postcard::from_bytes(encoded).map_err(|_| TicketError::Malformed)?;
        if ticket.members.len() > MAX_ROSTER {
            return Err(TicketError::TooLarge);
        }
        ticket
            .host
            .verify(&signed_message(encoded), &signature)
            .map_err(|_| TicketError::BadSignature)?;
        Ok(ticket)
    }

    pub fn lists(&self, device: &PublicId) -> bool {
        self.members.contains(device)
    }
}

fn signed_message(encoded: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + encoded.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(encoded);
    message
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ticket(host: &Identity, members: usize) -> RosterTicket {
        RosterTicket {
            room_tag: [5; 32],
            host: host.public(),
            epoch: 3,
            members: (0..members).map(|i| PublicId([u8::try_from(i % 256).unwrap(); 32])).collect(),
        }
    }

    #[test]
    fn a_sealed_ticket_opens_and_lists_its_members() {
        let host = Identity::generate();
        let opened = RosterTicket::open(&ticket(&host, 2).seal(&host).unwrap()).unwrap();
        assert_eq!(opened, ticket(&host, 2));
        assert!(opened.lists(&PublicId([1; 32])));
        assert!(!opened.lists(&PublicId([9; 32])));
    }

    #[test]
    fn forged_tampered_or_short_tickets_are_refused() {
        let host = Identity::generate();
        let other = Identity::generate();
        assert_eq!(ticket(&host, 1).seal(&other), Err(TicketError::BadSignature));

        let sealed = ticket(&host, 2).seal(&host).unwrap();
        let mut tampered = sealed.clone();
        tampered[1] ^= 1;
        assert!(RosterTicket::open(&tampered).is_err());
        assert_eq!(RosterTicket::open(&sealed[..20]), Err(TicketError::Malformed));
        assert_eq!(RosterTicket::open(&[]), Err(TicketError::Malformed));
        assert_eq!(RosterTicket::open(&vec![0; MAX_TICKET_LEN + 1]), Err(TicketError::TooLarge));
    }

    #[test]
    fn an_oversized_roster_is_refused() {
        let host = Identity::generate();
        assert_eq!(ticket(&host, MAX_ROSTER + 1).seal(&host), Err(TicketError::TooLarge));
    }
}
