//! Device identity: one Ed25519 key pair per device.
//!
//! The secret key never leaves the device. Other people see only [`PublicId`] and compare
//! safety numbers (see [`crate::safety`]) to confirm it.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand_core::{CryptoRngCore, OsRng};
use serde::{Deserialize, Serialize};

use crate::SecurityError;

/// A device's signing key. Wiped from memory on drop (ed25519-dalek zeroizes `SigningKey`).
pub struct Identity {
    key: SigningKey,
}

impl Identity {
    /// Generates a new identity from the operating system's random source.
    pub fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    /// Generates a new identity from the given random source.
    pub fn generate_with<R: CryptoRngCore + ?Sized>(rng: &mut R) -> Self {
        Self {
            key: SigningKey::generate(rng),
        }
    }

    /// Restores an identity from its 32-byte secret.
    pub fn from_secret(secret: &[u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(secret),
        }
    }

    pub fn public(&self) -> PublicId {
        PublicId(self.key.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("public", &self.public())
            .finish_non_exhaustive()
    }
}

/// A device's public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct PublicId(pub [u8; 32]);

impl PublicId {
    /// Verifies a signature with strict rules (rejects weak keys and malleable signatures).
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> Result<(), SecurityError> {
        let key = VerifyingKey::from_bytes(&self.0).map_err(|_| SecurityError::BadKey)?;
        key.verify_strict(message, &Signature::from_bytes(signature))
            .map_err(|_| SecurityError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_round_trip() {
        let id = Identity::generate();
        let sig = id.sign(b"hello");
        assert_eq!(id.public().verify(b"hello", &sig), Ok(()));
    }

    #[test]
    fn wrong_message_fails() {
        let id = Identity::generate();
        let sig = id.sign(b"hello");
        assert_eq!(
            id.public().verify(b"hellO", &sig),
            Err(SecurityError::BadSignature)
        );
    }

    #[test]
    fn other_key_fails() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = a.sign(b"hello");
        assert_eq!(
            b.public().verify(b"hello", &sig),
            Err(SecurityError::BadSignature)
        );
    }

    #[test]
    fn restore_from_secret_keeps_public_key() {
        let secret = [7u8; 32];
        assert_eq!(
            Identity::from_secret(&secret).public(),
            Identity::from_secret(&secret).public()
        );
    }

    #[test]
    fn debug_does_not_print_secret() {
        let id = Identity::from_secret(&[9u8; 32]);
        let text = format!("{id:?}");
        assert!(!text.contains("9, 9, 9"));
    }
}
