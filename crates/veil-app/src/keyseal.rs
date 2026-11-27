//! Sealing the device key and verified list, so a copied file gives nothing.
//!
//! Purpose: one [`seal`] and [`unseal`] pair for every platform. Windows uses DPAPI and
//! Android the Keystore, both through `veil_media`. macOS and Linux have no per-user file
//! sealing, so a random 32-byte wrapping key is kept in the OS keychain (Keychain on macOS,
//! the Secret Service on Linux, which GNOME Keyring and KWallet provide) and each file is
//! sealed with ChaCha20-Poly1305 under it.
//!
//! Invariants:
//! - [`Unseal::Unavailable`] means the keychain could not be asked: no Secret Service, a
//!   refused unlock, a damaged entry. The file may still open later, so the caller must
//!   leave it in place. Only [`Unseal::WrongKey`] lets the caller move a file aside.
//! - A wrapping key is made only while sealing, and only when the keychain says it has
//!   none. An existing one is never replaced, so earlier files keep opening.
//! - The file's label is bound as associated data, so the key file and the verified list
//!   cannot be swapped for each other.
//! - The wrapping key is kept in memory for the process, so saving the verified list does
//!   not ask the keychain (or the person, through an unlock prompt) again.

/// Why a sealed file did not open.
#[derive(Debug, PartialEq, Eq)]
pub enum Unseal {
    /// The file does not open for this user: another user's, another computer's, or damaged.
    WrongKey,
    /// The keychain could not be asked. The file may open on a later run.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    Unavailable(String),
}

/// Whether this build can seal files at all.
pub const CAN_SEAL: bool = cfg!(any(
    windows,
    target_os = "android",
    target_os = "linux",
    target_os = "macos"
));

/// What the person sees when a file does not open.
pub const WRONG_KEY: &str = if cfg!(windows) {
    "does not open for this Windows user"
} else if cfg!(target_os = "macos") {
    "does not open with the key in this Mac's keychain"
} else if cfg!(target_os = "linux") {
    "does not open with the key in this user's keyring"
} else {
    "does not open on this device"
};

#[cfg(windows)]
pub fn seal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, String> {
    veil_media::protect(bytes, label).map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn unseal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, Unseal> {
    veil_media::unprotect(bytes, label).map_err(|_| Unseal::WrongKey)
}

#[cfg(target_os = "android")]
pub fn seal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, String> {
    veil_media::ffi::android::protect(bytes, label, true).map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub fn unseal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, Unseal> {
    veil_media::ffi::android::protect(bytes, label, false).map_err(|_| Unseal::WrongKey)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn seal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, String> {
    let key = os::wrapping_key(true)?
        .ok_or_else(|| String::from("the keychain did not keep the new key"))?;
    wrap(&key, label, bytes)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn unseal(sealed: &[u8], label: &[u8]) -> Result<Vec<u8>, Unseal> {
    match os::wrapping_key(false) {
        Ok(Some(key)) => open(&key, label, sealed).ok_or(Unseal::WrongKey),
        // A file sealed under a key this keychain no longer has never opens again.
        Ok(None) => Err(Unseal::WrongKey),
        Err(e) => Err(Unseal::Unavailable(e)),
    }
}

// Never called: `CAN_SEAL` is false, so no key folder is given (keystore::key_dir).
#[cfg(not(any(windows, target_os = "android", target_os = "linux", target_os = "macos")))]
pub fn seal(_: &[u8], _: &[u8]) -> Result<Vec<u8>, String> {
    Err(String::from("saving keys is not built for this system"))
}

#[cfg(not(any(windows, target_os = "android", target_os = "linux", target_os = "macos")))]
pub fn unseal(_: &[u8], _: &[u8]) -> Result<Vec<u8>, Unseal> {
    Err(Unseal::WrongKey)
}

/// The keychain entry that holds the wrapping key.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os {
    use std::sync::{Arc, Mutex, PoisonError};

    use keyring_core::{CredentialStore, Entry, Error};
    use zeroize::Zeroize;

    const SERVICE: &str = "Veil";
    const ACCOUNT: &str = "file-sealing-key-v1";

    /// What the keychain said this run: its key, or why it could not be used.
    enum Cached {
        Key([u8; 32]),
        Failed(String),
    }

    // shortcut: the cached key is a plain array that lives until the process ends, like the
    // device key it protects. Wipe it on exit if Veil ever keeps running after sign-out.
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

    #[cfg(target_os = "linux")]
    fn entry() -> Result<Entry, Error> {
        let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
        let label = std::collections::HashMap::from([("label", "Veil: protects your device key")]);
        Entry::new_in_store_with_modifiers(&store, SERVICE, ACCOUNT, &label)
    }

    #[cfg(target_os = "macos")]
    fn entry() -> Result<Entry, Error> {
        let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
        Entry::new_in_store(&store, SERVICE, ACCOUNT)
    }

    /// The wrapping key, or `None` when the keychain has none. With `create`, a missing
    /// key is made and stored first. Any other keychain failure is an error, never a new key.
    ///
    /// A failure is kept for the rest of the run, so a refused unlock is asked once, not once
    /// per file.
    pub(super) fn wrapping_key(create: bool) -> Result<Option<[u8; 32]>, String> {
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        match &*cache {
            Some(Cached::Key(key)) => return Ok(Some(*key)),
            Some(Cached::Failed(why)) => return Err(why.clone()),
            None => {}
        }
        match ask(create) {
            Ok(Some(key)) => {
                *cache = Some(Cached::Key(key));
                Ok(Some(key))
            }
            // Not cached: a later save may still make the key.
            Ok(None) => Ok(None),
            Err(why) => {
                *cache = Some(Cached::Failed(why.clone()));
                Err(why)
            }
        }
    }

    fn ask(create: bool) -> Result<Option<[u8; 32]>, String> {
        let entry = entry().map_err(|e| unavailable(&e))?;
        match read(&entry) {
            Err(Error::NoEntry) if create => {}
            Err(Error::NoEntry) => return Ok(None),
            other => return other.map(Some).map_err(|e| unavailable(&e)),
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| e.to_string())?;
        entry.set_secret(&key).map_err(|e| unavailable(&e))?;
        key.zeroize();
        // Read back rather than trust our own copy: another Veil starting at the same moment
        // may have stored its key last, and both must seal with the one the keychain keeps.
        read(&entry).map(Some).map_err(|e| unavailable(&e))
    }

    fn read(entry: &Entry) -> Result<[u8; 32], Error> {
        let mut bytes = entry.get_secret()?;
        let key = <[u8; 32]>::try_from(bytes.as_slice());
        bytes.zeroize();
        key.map_err(|_| {
            Error::Invalid(
                String::from("secret"),
                String::from("the keychain entry for Veil has the wrong length"),
            )
        })
    }

    fn unavailable(e: &Error) -> String {
        let store = if cfg!(target_os = "macos") {
            "the macOS keychain"
        } else {
            "the keyring (Secret Service)"
        };
        format!("{store} could not be used: {e}")
    }
}

/// Sealed file layout: version, nonce, then ciphertext with its tag.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const VERSION: u8 = 1;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const NONCE_LEN: usize = 12;

/// Seals `bytes` under `key`, binding `label`.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn wrap(key: &[u8; 32], label: &[u8], bytes: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};

    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let sealed = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| String::from("bad sealing key"))?
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: bytes,
                aad: label,
            },
        )
        .map_err(|_| String::from("sealing failed"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Opens a file sealed by [`wrap`], or `None` if it does not open under `key` and `label`.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn open(key: &[u8; 32], label: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};

    let (&version, rest) = sealed.split_first()?;
    if version != VERSION {
        return None;
    }
    let (nonce, body) = rest.split_first_chunk::<NONCE_LEN>()?;
    ChaCha20Poly1305::new_from_slice(key)
        .ok()?
        .decrypt(
            &Nonce::from(*nonce),
            Payload {
                msg: body,
                aad: label,
            },
        )
        .ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn a_sealed_file_opens_with_its_key_and_label() {
        let sealed = wrap(&KEY, b"device", b"secret bytes").unwrap();
        assert_eq!(open(&KEY, b"device", &sealed).unwrap(), b"secret bytes");
        assert!(
            !sealed.windows(12).any(|w| w == b"secret bytes"),
            "not stored in the clear"
        );
    }

    #[test]
    fn it_does_not_open_with_another_key_label_or_change() {
        let sealed = wrap(&KEY, b"device", b"secret").unwrap();
        assert_eq!(open(&[8; 32], b"device", &sealed), None, "another key");
        assert_eq!(open(&KEY, b"verified", &sealed), None, "files cannot be swapped");
        for i in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert_eq!(open(&KEY, b"device", &bad), None, "byte {i} changed");
        }
    }

    #[test]
    fn short_or_foreign_files_do_not_open() {
        for bad in [&b""[..], &[VERSION][..], &[VERSION; 20][..], b"copied from elsewhere"] {
            assert_eq!(open(&KEY, b"device", bad), None);
        }
    }

    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        assert_ne!(wrap(&KEY, b"d", b"x").unwrap(), wrap(&KEY, b"d", b"x").unwrap());
    }
}
