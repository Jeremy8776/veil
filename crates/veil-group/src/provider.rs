//! The security backend openmls runs on.
//!
//! Purpose: `openmls_rust_crypto` pulls in `hpke-rs`, which is MPL-2.0, so Veil supplies its own
//! small backend. It supports exactly one ciphersuite,
//! `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`: X25519 HPKE (the `hpke` crate),
//! ChaCha20-Poly1305, SHA-256 and Ed25519. These match the primitives `veil-security` already uses.
//!
//! Invariants:
//! - Every other ciphersuite, hash, AEAD or signature scheme returns an error. Nothing panics.
//! - Randomness comes from one ChaCha20 generator seeded once from the operating system. Seeding
//!   is fallible and happens in [`VeilCrypto::new`], so later draws cannot fail.
//! - Secret outputs go into `SecretVLBytes` or `Zeroizing` buffers.

use std::sync::Mutex;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use hpke::aead::ChaCha20Poly1305 as HpkeChaCha;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem as _, OpModeR, OpModeS, Serializable};
use openmls_memory_storage::MemoryStorage;
use openmls_traits::OpenMlsProvider;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::random::OpenMlsRand;
use openmls_traits::types::{
    AeadType, Ciphersuite, CryptoError, ExporterSecret, HashType, HpkeAeadType, HpkeCiphertext,
    HpkeConfig, HpkeKdfType, HpkeKemType, HpkeKeyPair, KemOutput, SignatureScheme,
};
use rand_chacha::ChaCha20Rng;
use rand_core::{OsRng, RngCore, SeedableRng, TryRngCore};
use sha2::{Digest, Sha256};
use tls_codec::SecretVLBytes;
use zeroize::Zeroizing;

/// The one ciphersuite Veil groups use.
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

const SUITE_HPKE: HpkeConfig = HpkeConfig(
    HpkeKemType::DhKem25519,
    HpkeKdfType::HkdfSha256,
    HpkeAeadType::ChaCha20Poly1305,
);

/// Error from the random generator: only a poisoned lock can cause it.
#[derive(Debug)]
pub struct RandError;

impl std::fmt::Display for RandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("random generator unavailable")
    }
}

impl std::error::Error for RandError {}

pub struct VeilCrypto {
    rng: Mutex<ChaCha20Rng>,
}

impl VeilCrypto {
    /// Seeds the generator from the operating system. Fails only if the OS has no randomness.
    pub fn new() -> Result<Self, CryptoError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        OsRng
            .try_fill_bytes(seed.as_mut())
            .map_err(|_| CryptoError::InsufficientRandomness)?;
        Ok(Self {
            rng: Mutex::new(ChaCha20Rng::from_seed(*seed)),
        })
    }

    fn with_rng<T>(&self, f: impl FnOnce(&mut ChaCha20Rng) -> T) -> Result<T, CryptoError> {
        let mut rng = self.rng.lock().map_err(|_| CryptoError::InsufficientRandomness)?;
        Ok(f(&mut rng))
    }
}

fn check_hpke(config: &HpkeConfig) -> Result<(), CryptoError> {
    let HpkeConfig(kem, kdf, aead) = config;
    if *kem == SUITE_HPKE.0 && *kdf == SUITE_HPKE.1 && *aead == SUITE_HPKE.2 {
        Ok(())
    } else {
        Err(CryptoError::UnsupportedCiphersuite)
    }
}

fn check_hash(hash: HashType) -> Result<(), CryptoError> {
    match hash {
        HashType::Sha2_256 => Ok(()),
        _ => Err(CryptoError::UnsupportedHashAlgorithm),
    }
}

fn chacha(alg: AeadType, key: &[u8], nonce: &[u8]) -> Result<ChaCha20Poly1305, CryptoError> {
    if alg != AeadType::ChaCha20Poly1305 {
        return Err(CryptoError::UnsupportedAeadAlgorithm);
    }
    if nonce.len() != 12 {
        return Err(CryptoError::InvalidLength);
    }
    ChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::InvalidLength)
}

fn hpke_public(bytes: &[u8]) -> Result<<X25519HkdfSha256 as hpke::Kem>::PublicKey, CryptoError> {
    Deserializable::from_bytes(bytes).map_err(|_| CryptoError::InvalidPublicKey)
}

fn hpke_private(bytes: &[u8]) -> Result<<X25519HkdfSha256 as hpke::Kem>::PrivateKey, CryptoError> {
    Deserializable::from_bytes(bytes).map_err(|_| CryptoError::CryptoLibraryError)
}

fn hpke_encapped(bytes: &[u8]) -> Result<<X25519HkdfSha256 as hpke::Kem>::EncappedKey, CryptoError> {
    Deserializable::from_bytes(bytes).map_err(|_| CryptoError::InvalidLength)
}

impl OpenMlsCrypto for VeilCrypto {
    fn supports(&self, ciphersuite: Ciphersuite) -> Result<(), CryptoError> {
        if ciphersuite == CIPHERSUITE {
            Ok(())
        } else {
            Err(CryptoError::UnsupportedCiphersuite)
        }
    }

    fn supported_ciphersuites(&self) -> Vec<Ciphersuite> {
        vec![CIPHERSUITE]
    }

    fn hkdf_extract(&self, hash: HashType, salt: &[u8], ikm: &[u8]) -> Result<SecretVLBytes, CryptoError> {
        check_hash(hash)?;
        let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
        Ok(prk.as_slice().into())
    }

    fn hmac(&self, hash: HashType, key: &[u8], message: &[u8]) -> Result<SecretVLBytes, CryptoError> {
        check_hash(hash)?;
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidLength)?;
        mac.update(message);
        Ok(mac.finalize().into_bytes().as_slice().into())
    }

    fn hkdf_expand(
        &self,
        hash: HashType,
        prk: &[u8],
        info: &[u8],
        okm_len: usize,
    ) -> Result<SecretVLBytes, CryptoError> {
        check_hash(hash)?;
        let hk = Hkdf::<Sha256>::from_prk(prk).map_err(|_| CryptoError::HkdfOutputLengthInvalid)?;
        let mut okm = Zeroizing::new(vec![0u8; okm_len]);
        hk.expand(info, &mut okm)
            .map_err(|_| CryptoError::HkdfOutputLengthInvalid)?;
        Ok(okm.as_slice().into())
    }

    fn hash(&self, hash: HashType, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        check_hash(hash)?;
        Ok(Sha256::digest(data).to_vec())
    }

    fn aead_encrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        data: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        chacha(alg, key, nonce)?
            .encrypt(nonce.into(), Payload { msg: data, aad })
            .map_err(|_| CryptoError::CryptoLibraryError)
    }

    fn aead_decrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        ct_tag: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        chacha(alg, key, nonce)?
            .decrypt(nonce.into(), Payload { msg: ct_tag, aad })
            .map_err(|_| CryptoError::AeadDecryptionError)
    }

    fn signature_key_gen(&self, alg: SignatureScheme) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        if alg != SignatureScheme::ED25519 {
            return Err(CryptoError::UnsupportedSignatureScheme);
        }
        let mut secret = Zeroizing::new([0u8; 32]);
        self.with_rng(|rng| rng.fill_bytes(secret.as_mut()))?;
        let key = SigningKey::from_bytes(&secret);
        Ok((secret.to_vec(), key.verifying_key().to_bytes().to_vec()))
    }

    fn verify_signature(
        &self,
        alg: SignatureScheme,
        data: &[u8],
        pk: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError> {
        if alg != SignatureScheme::ED25519 {
            return Err(CryptoError::UnsupportedSignatureScheme);
        }
        let key = VerifyingKey::try_from(pk).map_err(|_| CryptoError::InvalidPublicKey)?;
        let signature =
            ed25519_dalek::Signature::from_slice(signature).map_err(|_| CryptoError::InvalidSignature)?;
        key.verify_strict(data, &signature)
            .map_err(|_| CryptoError::InvalidSignature)
    }

    fn sign(&self, alg: SignatureScheme, data: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if alg != SignatureScheme::ED25519 {
            return Err(CryptoError::UnsupportedSignatureScheme);
        }
        let secret: Zeroizing<[u8; 32]> =
            Zeroizing::new(key.try_into().map_err(|_| CryptoError::InvalidLength)?);
        Ok(SigningKey::from_bytes(&secret).sign(data).to_bytes().to_vec())
    }

    fn hpke_seal(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        aad: &[u8],
        ptxt: &[u8],
    ) -> Result<HpkeCiphertext, CryptoError> {
        check_hpke(&config)?;
        let pk = hpke_public(pk_r)?;
        let (enc, ciphertext) = self.with_rng(|rng| {
            hpke::single_shot_seal::<HpkeChaCha, HkdfSha256, X25519HkdfSha256, _>(
                &OpModeS::Base,
                &pk,
                info,
                ptxt,
                aad,
                rng,
            )
        })?
        .map_err(|_| CryptoError::CryptoLibraryError)?;
        Ok(HpkeCiphertext {
            kem_output: enc.to_bytes().to_vec().into(),
            ciphertext: ciphertext.into(),
        })
    }

    fn hpke_open(
        &self,
        config: HpkeConfig,
        input: &HpkeCiphertext,
        sk_r: &[u8],
        info: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        check_hpke(&config)?;
        let sk = hpke_private(sk_r)?;
        let enc = hpke_encapped(input.kem_output.as_slice())?;
        hpke::single_shot_open::<HpkeChaCha, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            &sk,
            &enc,
            info,
            input.ciphertext.as_slice(),
            aad,
        )
        .map_err(|_| CryptoError::HpkeDecryptionError)
    }

    fn hpke_setup_sender_and_export(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<(KemOutput, ExporterSecret), CryptoError> {
        check_hpke(&config)?;
        let pk = hpke_public(pk_r)?;
        let (enc, context) = self
            .with_rng(|rng| {
                hpke::setup_sender::<HpkeChaCha, HkdfSha256, X25519HkdfSha256, _>(
                    &OpModeS::Base,
                    &pk,
                    info,
                    rng,
                )
            })?
            .map_err(|_| CryptoError::SenderSetupError)?;
        let mut out = Zeroizing::new(vec![0u8; exporter_length]);
        context
            .export(exporter_context, &mut out)
            .map_err(|_| CryptoError::ExporterError)?;
        Ok((enc.to_bytes().to_vec(), out.as_slice().to_vec().into()))
    }

    fn hpke_setup_receiver_and_export(
        &self,
        config: HpkeConfig,
        enc: &[u8],
        sk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<ExporterSecret, CryptoError> {
        check_hpke(&config)?;
        let sk = hpke_private(sk_r)?;
        let enc = hpke_encapped(enc)?;
        let context = hpke::setup_receiver::<HpkeChaCha, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            &sk,
            &enc,
            info,
        )
        .map_err(|_| CryptoError::ReceiverSetupError)?;
        let mut out = Zeroizing::new(vec![0u8; exporter_length]);
        context
            .export(exporter_context, &mut out)
            .map_err(|_| CryptoError::ExporterError)?;
        Ok(out.as_slice().to_vec().into())
    }

    fn derive_hpke_keypair(&self, config: HpkeConfig, ikm: &[u8]) -> Result<HpkeKeyPair, CryptoError> {
        check_hpke(&config)?;
        let (private, public) = X25519HkdfSha256::derive_keypair(ikm);
        let private = Zeroizing::new(private.to_bytes().to_vec());
        Ok(HpkeKeyPair {
            private: private.as_slice().into(),
            public: public.to_bytes().to_vec(),
        })
    }
}

impl OpenMlsRand for VeilCrypto {
    type Error = RandError;

    fn random_array<const N: usize>(&self) -> Result<[u8; N], Self::Error> {
        let mut out = [0u8; N];
        self.with_rng(|rng| rng.fill_bytes(&mut out))
            .map_err(|_| RandError)?;
        Ok(out)
    }

    fn random_vec(&self, len: usize) -> Result<Vec<u8>, Self::Error> {
        let mut out = vec![0u8; len];
        self.with_rng(|rng| rng.fill_bytes(&mut out))
            .map_err(|_| RandError)?;
        Ok(out)
    }
}

/// Security backend plus in-memory key storage for one participant.
///
/// shortcut: keys live in memory only, so a restarted app rejoins as a new member. Upgrade to a
/// persistent `StorageProvider` when calls must survive an app restart.
pub struct VeilProvider {
    crypto: VeilCrypto,
    storage: MemoryStorage,
}

impl VeilProvider {
    pub fn new() -> Result<Self, CryptoError> {
        Ok(Self {
            crypto: VeilCrypto::new()?,
            storage: MemoryStorage::default(),
        })
    }
}

impl OpenMlsProvider for VeilProvider {
    type CryptoProvider = VeilCrypto;
    type RandProvider = VeilCrypto;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hpke_round_trip_and_export_agree() {
        let crypto = VeilCrypto::new().unwrap();
        let pair = crypto.derive_hpke_keypair(SUITE_HPKE, &[7; 32]).unwrap();
        let sealed = crypto
            .hpke_seal(SUITE_HPKE, &pair.public, b"info", b"aad", b"hello")
            .unwrap();
        let opened = crypto
            .hpke_open(SUITE_HPKE, &sealed, &pair.private, b"info", b"aad")
            .unwrap();
        assert_eq!(opened, b"hello");
        assert!(crypto
            .hpke_open(SUITE_HPKE, &sealed, &pair.private, b"info", b"other")
            .is_err());

        let (enc, sent) = crypto
            .hpke_setup_sender_and_export(SUITE_HPKE, &pair.public, b"i", b"ctx", 32)
            .unwrap();
        let got = crypto
            .hpke_setup_receiver_and_export(SUITE_HPKE, &enc, &pair.private, b"i", b"ctx", 32)
            .unwrap();
        assert_eq!(&*sent, &*got);
        let sealed_again = crypto
            .hpke_seal(SUITE_HPKE, &pair.public, b"info", b"aad", b"hello")
            .unwrap();
        assert_ne!(sealed.kem_output, sealed_again.kem_output, "ephemeral keys must differ");
    }

    #[test]
    fn signatures_round_trip_and_reject_tampering() {
        let crypto = VeilCrypto::new().unwrap();
        let (sk, pk) = crypto.signature_key_gen(SignatureScheme::ED25519).unwrap();
        let sig = crypto.sign(SignatureScheme::ED25519, b"data", &sk).unwrap();
        assert!(crypto.verify_signature(SignatureScheme::ED25519, b"data", &pk, &sig).is_ok());
        assert!(crypto.verify_signature(SignatureScheme::ED25519, b"dat4", &pk, &sig).is_err());
    }

    #[test]
    fn other_algorithms_are_refused() {
        let crypto = VeilCrypto::new().unwrap();
        assert!(crypto
            .supports(Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519)
            .is_err());
        assert!(crypto.hash(HashType::Sha2_512, b"x").is_err());
        assert!(crypto
            .aead_encrypt(AeadType::Aes128Gcm, &[0; 16], b"x", &[0; 12], b"")
            .is_err());
        assert!(crypto
            .signature_key_gen(SignatureScheme::ECDSA_SECP256R1_SHA256)
            .is_err());
    }
}
