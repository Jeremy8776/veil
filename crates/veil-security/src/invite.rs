//! Signed invite links.
//!
//! An invite tells a joiner where to find the host and proves the host made it. It carries a
//! one-time join token, never a media key: the host still admits each joiner from a lobby and
//! adds them to the MLS group, which is what grants access to media.
//!
//! Link format: `veil:` + base64url(postcard(Invite) || Ed25519 signature). The signature
//! covers a domain tag plus the encoded invite. Links are bounded in size before decoding.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRng;
use serde::{Deserialize, Serialize};

use crate::{SecurityError, Identity, PublicId};

pub const PREFIX: &str = "veil:";
pub const VERSION: u8 = 1;
pub const MAX_LINK_LEN: usize = 2_048;
pub const MAX_ADDRS: usize = 8;
pub const MAX_ADDR_LEN: usize = 255;
const SIGNATURE_LEN: usize = 64;
const DOMAIN: &[u8] = b"veil-invite-v1";
const PASS_DOMAIN: &[u8] = b"veil-lobby-pass-v1";

/// The lobby pass for a join token: what a joiner shows the rendezvous node to register in
/// the host's room before it knocks. A one-way hash, so the node that sees the pass cannot
/// knock with the token itself.
pub fn lobby_pass(join_token: &[u8; 16]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::new()
        .chain_update(PASS_DOMAIN)
        .chain_update(join_token)
        .finalize()
        .into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub version: u8,
    pub room_id: [u8; 16],
    /// The host's device key. The link must be signed by this key.
    pub host: PublicId,
    /// Addresses to try for a direct connection, for example `203.0.113.7:4433`.
    pub addrs: Vec<String>,
    /// Optional relay for networks that block direct connections. Sees ciphertext only.
    pub relay: Option<String>,
    /// Unix time in seconds after which the link is refused.
    pub expires_unix: u64,
    /// One-time token the host checks and then burns.
    pub join_token: [u8; 16],
}

impl Invite {
    /// Creates an invite for `host` with a fresh room id and join token.
    pub fn new<R: CryptoRng + ?Sized>(
        host: &Identity,
        addrs: Vec<String>,
        relay: Option<String>,
        expires_unix: u64,
        rng: &mut R,
    ) -> Result<Self, SecurityError> {
        let mut room_id = [0u8; 16];
        let mut join_token = [0u8; 16];
        rng.fill_bytes(&mut room_id);
        rng.fill_bytes(&mut join_token);
        let invite = Self {
            version: VERSION,
            room_id,
            host: host.public(),
            addrs,
            relay,
            expires_unix,
            join_token,
        };
        invite.validate()?;
        Ok(invite)
    }

    fn validate(&self) -> Result<(), SecurityError> {
        if self.version != VERSION {
            return Err(SecurityError::Malformed("version"));
        }
        if self.addrs.len() > MAX_ADDRS {
            return Err(SecurityError::Malformed("too many addresses"));
        }
        let too_long = |a: &String| a.is_empty() || a.len() > MAX_ADDR_LEN;
        if self.addrs.iter().any(too_long) || self.relay.as_ref().is_some_and(too_long) {
            return Err(SecurityError::Malformed("address length"));
        }
        Ok(())
    }

    /// Encodes and signs the invite. `host` must be the identity named in the invite.
    pub fn to_link(&self, host: &Identity) -> Result<String, SecurityError> {
        if host.public() != self.host {
            return Err(SecurityError::BadKey);
        }
        self.validate()?;
        let mut bytes =
            postcard::to_allocvec(self).map_err(|_| SecurityError::Malformed("encode"))?;
        let signature = host.sign(&signed_message(&bytes));
        bytes.extend_from_slice(&signature);
        let link = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bytes));
        if link.len() > MAX_LINK_LEN {
            return Err(SecurityError::TooLarge);
        }
        Ok(link)
    }

    /// Decodes a link, verifies the host's signature and checks expiry against `now_unix`.
    pub fn from_link(link: &str, now_unix: u64) -> Result<Self, SecurityError> {
        if link.len() > MAX_LINK_LEN {
            return Err(SecurityError::TooLarge);
        }
        let body = link
            .trim()
            .strip_prefix(PREFIX)
            .ok_or(SecurityError::Malformed("prefix"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| SecurityError::Malformed("base64"))?;
        let split = bytes
            .len()
            .checked_sub(SIGNATURE_LEN)
            .filter(|n| *n > 0)
            .ok_or(SecurityError::Malformed("length"))?;
        let (encoded, sig_bytes) = bytes.split_at(split);
        let signature: [u8; SIGNATURE_LEN] = sig_bytes
            .try_into()
            .map_err(|_| SecurityError::Malformed("signature"))?;
        let invite: Self =
            postcard::from_bytes(encoded).map_err(|_| SecurityError::Malformed("decode"))?;
        invite.host.verify(&signed_message(encoded), &signature)?;
        invite.validate()?;
        if now_unix >= invite.expires_unix {
            return Err(SecurityError::Expired);
        }
        Ok(invite)
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
    use getrandom::SysRng;
    use rand_core::UnwrapErr;

    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn sample(host: &Identity) -> Invite {
        Invite::new(
            host,
            vec!["203.0.113.7:4433".into(), "[2001:db8::1]:4433".into()],
            Some("relay.example.org:4433".into()),
            NOW + 3_600,
            &mut UnwrapErr(SysRng),
        )
        .unwrap()
    }

    #[test]
    fn round_trip() {
        let host = Identity::generate();
        let invite = sample(&host);
        let link = invite.to_link(&host).unwrap();
        assert!(link.starts_with(PREFIX));
        assert_eq!(Invite::from_link(&link, NOW).unwrap(), invite);
    }

    #[test]
    fn tampered_body_is_refused() {
        let host = Identity::generate();
        let link = sample(&host).to_link(&host).unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(&link[PREFIX.len()..]).unwrap();
        bytes[3] ^= 0x01;
        let tampered = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bytes));
        assert!(Invite::from_link(&tampered, NOW).is_err());
    }

    #[test]
    fn invite_re_signed_by_someone_else_is_refused() {
        let host = Identity::generate();
        let attacker = Identity::generate();
        let mut invite = sample(&host);
        invite.addrs = vec!["198.51.100.9:4433".into()];
        // The attacker cannot sign as the host.
        assert_eq!(invite.to_link(&attacker), Err(SecurityError::BadKey));
    }

    #[test]
    fn expired_link_is_refused() {
        let host = Identity::generate();
        let link = sample(&host).to_link(&host).unwrap();
        assert_eq!(
            Invite::from_link(&link, NOW + 3_600),
            Err(SecurityError::Expired)
        );
    }

    #[test]
    fn wrong_prefix_and_garbage_are_refused() {
        assert_eq!(
            Invite::from_link("https://example.org", NOW),
            Err(SecurityError::Malformed("prefix"))
        );
        assert!(Invite::from_link("veil:!!!", NOW).is_err());
        assert!(Invite::from_link("veil:", NOW).is_err());
    }

    #[test]
    fn oversized_link_is_refused_before_decoding() {
        let link = format!("{PREFIX}{}", "A".repeat(MAX_LINK_LEN));
        assert_eq!(Invite::from_link(&link, NOW), Err(SecurityError::TooLarge));
    }

    #[test]
    fn lobby_pass_is_stable_and_hides_the_token() {
        let token = [9u8; 16];
        assert_eq!(lobby_pass(&token), lobby_pass(&token));
        assert_ne!(lobby_pass(&token), lobby_pass(&[8u8; 16]));
        assert!(!lobby_pass(&token).windows(16).any(|w| w == token));
    }

    #[test]
    fn too_many_addresses_are_refused() {
        let host = Identity::generate();
        let addrs = vec!["198.51.100.1:1".to_string(); MAX_ADDRS + 1];
        assert!(Invite::new(&host, addrs, None, NOW, &mut UnwrapErr(SysRng)).is_err());
    }
}
