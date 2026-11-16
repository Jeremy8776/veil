//! DPAPI sealing: round trip, label binding, tamper refusal.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use veil_media::{protect, unprotect};

#[test]
fn sealed_data_opens_only_with_its_label_and_unchanged() {
    let secret = [7u8; 32];
    let sealed = protect(&secret, b"veil test").unwrap();
    assert!(!sealed.windows(32).any(|w| w == secret), "the secret is not in the clear");
    assert_eq!(unprotect(&sealed, b"veil test").unwrap(), secret);
    assert!(unprotect(&sealed, b"another purpose").is_err());
    let mut changed = sealed.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert!(unprotect(&changed, b"veil test").is_err());
    assert!(unprotect(b"not sealed", b"veil test").is_err());
}
