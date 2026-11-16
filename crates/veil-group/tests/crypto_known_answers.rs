//! Known-answer tests for the MLS crypto provider.
//!
//! Purpose: openmls derives every group secret, key package and welcome through `VeilCrypto`.
//! These tests pin its outputs (HKDF, HMAC, SHA-256, ChaCha20-Poly1305, Ed25519, HPKE key
//! derivation, opening and export) so a crate upgrade cannot change them and split clients on
//! different versions. Expected values were captured from the RustCrypto 0.10 generation
//! (hpke 0.13) before the upgrade to the next generation.
#![allow(clippy::unwrap_used)]

use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::types::{
    AeadType, HashType, HpkeAeadType, HpkeCiphertext, HpkeConfig, HpkeKdfType, HpkeKemType,
    SignatureScheme,
};
use veil_group::provider::VeilCrypto;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const SUITE: HpkeConfig = HpkeConfig(
    HpkeKemType::DhKem25519,
    HpkeKdfType::HkdfSha256,
    HpkeAeadType::ChaCha20Poly1305,
);

#[test]
fn hash_family_is_fixed() {
    let c = VeilCrypto::new().unwrap();
    let prk = c.hkdf_extract(HashType::Sha2_256, b"salt", b"ikm").unwrap();
    let okm = c.hkdf_expand(HashType::Sha2_256, prk.as_slice(), b"info", 42).unwrap();
    let mac = c.hmac(HashType::Sha2_256, b"key", b"message").unwrap();
    let digest = c.hash(HashType::Sha2_256, b"veil").unwrap();
    assert_eq!(hex(prk.as_slice()), "6c1a16bbfd2d010d2b2508af08a0cfb6a6f8445e71e209e4daabbe6d1bebf402");
    assert_eq!(hex(okm.as_slice()), "fe8f9615d2374c0d17f77d1aeaf408c2e75fe0466073d0def23c733e2f862dfd6814c9254418fa112fe8");
    assert_eq!(hex(mac.as_slice()), "6e9ef29b75fffc5b7abae527d58fdadb2fe42e7219011976917343065f58ed4a");
    assert_eq!(hex(&digest), "01979b66ee1794c473f53cafff0889e714aac28b2515e3572072424b919634f3");
}

#[test]
fn aead_is_fixed() {
    let c = VeilCrypto::new().unwrap();
    let sealed = c
        .aead_encrypt(AeadType::ChaCha20Poly1305, &[9; 32], b"plain", &[8; 12], b"aad")
        .unwrap();
    assert_eq!(hex(&sealed), "c5b7597c0d2e638bcb13dcc9a4f150a8927a5e10ee");
    let opened = c
        .aead_decrypt(AeadType::ChaCha20Poly1305, &[9; 32], &sealed, &[8; 12], b"aad")
        .unwrap();
    assert_eq!(opened, b"plain");
}

#[test]
fn signature_is_fixed() {
    let c = VeilCrypto::new().unwrap();
    let sig = c.sign(SignatureScheme::ED25519, b"data", &[1; 32]).unwrap();
    assert_eq!(hex(&sig), "2363f3b196876e83b08fd6a9d1718c462c2af748bfadc1154acf950350181e94031bbab04afb4deb379074dde649f97b91271544dfea2e4f1465337cbb67280c");
}

#[test]
fn hpke_key_derivation_is_fixed() {
    let c = VeilCrypto::new().unwrap();
    let pair = c.derive_hpke_keypair(SUITE, &[7; 32]).unwrap();
    assert_eq!(hex(&pair.public), "5fd47633b5e43fca7bad1bfbe424feacb175d9d4d4a30f166dbd30636f0b446b");
    // hpke 0.15 stores the private key clamped, as RFC 9180 section 7.1.2 asks. hpke 0.13
    // stored the same scalar unclamped (`UNCLAMPED_PRIVATE`). X25519 clamps on use, so both
    // encodings are the same key: same public key, and both open the same ciphertexts (see
    // `old_unclamped_private_key_still_opens`). Private keys never leave the device.
    assert_eq!(hex(pair.private.as_ref()), CLAMPED_PRIVATE);
    let mut clamped = unhex(UNCLAMPED_PRIVATE);
    clamped[0] &= 0xF8;
    clamped[31] = (clamped[31] & 0x7F) | 0x40;
    assert_eq!(hex(&clamped), CLAMPED_PRIVATE);
}

const UNCLAMPED_PRIVATE: &str = "896615161d5c4dab6be99bb69b180f22c86eb0ac1b8912e958ed5dd08813573b";
const CLAMPED_PRIVATE: &str = "886615161d5c4dab6be99bb69b180f22c86eb0ac1b8912e958ed5dd08813577b";

#[test]
fn old_unclamped_private_key_still_opens() {
    let c = VeilCrypto::new().unwrap();
    let sealed = HpkeCiphertext {
        kem_output: unhex(SEALED_KEM).into(),
        ciphertext: unhex(SEALED_CT).into(),
    };
    let opened = c
        .hpke_open(SUITE, &sealed, &unhex(UNCLAMPED_PRIVATE), b"info", b"aad")
        .unwrap();
    assert_eq!(opened, b"hello");
}

// Sealing is randomised, so these pin the receiver side: a ciphertext and an encapsulation made
// by the old build must still open, and export the same secret, on the new one.
const SEALED_KEM: &str = "1f7e5cf48ed4ae03c2d2daf687042cdfb5c80d8dffe24aa9b23bc7c090194c61";
const SEALED_CT: &str = "136f54fc7337a9bd22cfaf8d01448d1ea3bfddf6e1";
const EXPORT_ENC: &str = "1a697ee33ec1541eab9243c025d509d78b9a0faf6e55e85598bbf7eaf580ef76";
const EXPORT_SECRET: &str = "276a770837003575e0810c063fab2adfd1016e21cab0d805787ee8d5a2a4d709";

#[test]
fn hpke_open_and_export_are_fixed() {
    let c = VeilCrypto::new().unwrap();
    let pair = c.derive_hpke_keypair(SUITE, &[7; 32]).unwrap();
    let sealed = HpkeCiphertext {
        kem_output: unhex(SEALED_KEM).into(),
        ciphertext: unhex(SEALED_CT).into(),
    };
    let opened = c.hpke_open(SUITE, &sealed, &pair.private, b"info", b"aad").unwrap();
    assert_eq!(opened, b"hello");
    let secret = c
        .hpke_setup_receiver_and_export(SUITE, &unhex(EXPORT_ENC), &pair.private, b"i", b"ctx", 32)
        .unwrap();
    assert_eq!(hex(&secret), EXPORT_SECRET);
}
