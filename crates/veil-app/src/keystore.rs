//! Your device key and the keys you have verified, kept between calls.
//!
//! Purpose: one device key for every call, so a safety number checked once still means
//! something next time, and a list of keys you marked as verified after comparing safety
//! numbers. Both live in `%LOCALAPPDATA%\Veil`, sealed with Windows DPAPI
//! (`veil_media::protect`), so they open only for the same Windows user and a copied file
//! gives nothing.
//!
//! Invariants:
//! - Verified never admits anyone: it is a label for the host, who still decides.
//! - A file that does not open (another user, corrupt) is moved aside to `*.unreadable`,
//!   never deleted, and a fresh one is started. The reason is kept for the home screen.
//! - Files are written to a temporary name and renamed, so a crash never leaves half a key.
//! - With no `LOCALAPPDATA`, or off Windows, nothing is saved: the key lasts one run.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use veil_security::{Identity, PublicId};

const KEY_FILE: &str = "device.key";
const TRUSTED_FILE: &str = "verified.keys";
const KEY_LABEL: &[u8] = b"veil device key v1";
const TRUSTED_LABEL: &[u8] = b"veil verified keys v1";
/// Keys you may mark as verified. Far more than anyone calls.
pub const MAX_TRUSTED: usize = 10_000;

/// Where Veil keeps its files, if anywhere.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(|base| PathBuf::from(base).join("Veil"))
}

pub struct KeyStore {
    dir: Option<PathBuf>,
    secret: [u8; 32],
    trusted: BTreeSet<PublicId>,
    /// Something that went wrong reading or writing, for the home screen.
    pub problem: Option<String>,
}

impl KeyStore {
    /// Opens the store in `dir`, making a device key on first use.
    pub fn open(dir: Option<PathBuf>) -> Self {
        let mut problem = None;
        let secret = match dir.as_deref().map(load_key) {
            Some(Ok(Some(secret))) => Some(secret),
            Some(Ok(None)) | None => None,
            Some(Err(e)) => {
                problem = Some(e);
                None
            }
        };
        let trusted = match dir.as_deref().map(load_trusted) {
            Some(Ok(keys)) => keys,
            Some(Err(e)) => {
                problem.get_or_insert(e);
                BTreeSet::new()
            }
            None => BTreeSet::new(),
        };
        let mut store = Self {
            dir,
            secret: [0; 32],
            trusted,
            problem,
        };
        match secret {
            Some(secret) => store.secret = secret,
            None => store.renew(),
        }
        store
    }

    /// A copy of your device key, for one call.
    pub fn identity(&self) -> Identity {
        Identity::from_secret(&self.secret)
    }

    pub fn public(&self) -> PublicId {
        self.identity().public()
    }

    /// Makes a new device key and saves it. People who verified the old one see you as new.
    pub fn renew(&mut self) {
        let fresh = Identity::generate();
        self.secret = *fresh.secret_bytes();
        if let Some(dir) = &self.dir
            && let Err(e) = save(dir, KEY_FILE, &self.secret, KEY_LABEL)
        {
            self.problem = Some(format!("Your device key could not be saved: {e}"));
        }
    }

    pub fn is_verified(&self, key: &PublicId) -> bool {
        self.trusted.contains(key)
    }

    /// Marks `key` as verified, or forgets it, and saves the list.
    pub fn set_verified(&mut self, key: PublicId, verified: bool) {
        let changed = if verified {
            self.trusted.len() < MAX_TRUSTED && self.trusted.insert(key)
        } else {
            self.trusted.remove(&key)
        };
        if !changed {
            return;
        }
        let bytes: Vec<u8> = self.trusted.iter().flat_map(|k| k.0).collect();
        if let Some(dir) = &self.dir
            && let Err(e) = save(dir, TRUSTED_FILE, &bytes, TRUSTED_LABEL)
        {
            self.problem = Some(format!("Your verified list could not be saved: {e}"));
        }
    }
}

impl Drop for KeyStore {
    fn drop(&mut self) {
        // Best effort: the key also sits in the identity copies handed to calls.
        self.secret = [0; 32];
    }
}

/// `Ok(None)` when there is no key yet.
fn load_key(dir: &Path) -> Result<Option<[u8; 32]>, String> {
    let Some(bytes) = load(dir, KEY_FILE, KEY_LABEL)? else {
        return Ok(None);
    };
    match <[u8; 32]>::try_from(bytes.as_slice()) {
        Ok(secret) => Ok(Some(secret)),
        Err(_) => Err(set_aside(dir, KEY_FILE, "has the wrong length")),
    }
}

fn load_trusted(dir: &Path) -> Result<BTreeSet<PublicId>, String> {
    let Some(bytes) = load(dir, TRUSTED_FILE, TRUSTED_LABEL)? else {
        return Ok(BTreeSet::new());
    };
    if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_TRUSTED {
        return Err(set_aside(dir, TRUSTED_FILE, "has the wrong length"));
    }
    Ok(bytes.as_chunks::<32>().0.iter().map(|c| PublicId(*c)).collect())
}

/// Reads and opens `name`. A missing file is `Ok(None)`; one that does not open is moved
/// aside and reported.
fn load(dir: &Path, name: &str, label: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let sealed = match fs::read(dir.join(name)) {
        Ok(sealed) => sealed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{name} could not be read: {e}")),
    };
    match unseal(&sealed, label) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(_) => Err(set_aside(dir, name, "does not open for this Windows user")),
    }
}

/// Moves a bad file out of the way, keeping it, and says what happened.
fn set_aside(dir: &Path, name: &str, why: &str) -> String {
    let aside = dir.join(format!("{name}.unreadable"));
    let moved = fs::rename(dir.join(name), &aside).is_ok();
    let kept = if moved { " It was kept as" } else { " It could not be moved to" };
    format!("{name} {why}, so a new one was started.{kept} {}.", aside.display())
}

fn save(dir: &Path, name: &str, bytes: &[u8], label: &[u8]) -> Result<(), String> {
    let sealed = seal(bytes, label)?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("{name}.new"));
    fs::write(&temp, sealed).map_err(|e| e.to_string())?;
    fs::rename(&temp, dir.join(name)).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn seal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, String> {
    veil_media::protect(bytes, label).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn unseal(bytes: &[u8], label: &[u8]) -> Result<Vec<u8>, String> {
    veil_media::unprotect(bytes, label).map_err(|e| e.to_string())
}

// shortcut: no sealing off Windows, so nothing is saved there. Add the platform's keychain
// with the macOS and Linux back ends.
#[cfg(not(windows))]
fn seal(_: &[u8], _: &[u8]) -> Result<Vec<u8>, String> {
    Err(String::from("saving keys is built for Windows only"))
}

#[cfg(not(windows))]
fn unseal(_: &[u8], _: &[u8]) -> Result<Vec<u8>, String> {
    Err(String::from("saving keys is built for Windows only"))
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-keystore-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_key_and_verified_list_last_between_runs() {
        let dir = temp_dir("lasts");
        let mut first = KeyStore::open(Some(dir.clone()));
        assert!(first.problem.is_none(), "{:?}", first.problem);
        let me = first.public();
        let friend = Identity::generate().public();
        first.set_verified(friend, true);
        drop(first);

        let mut again = KeyStore::open(Some(dir.clone()));
        assert_eq!(again.public(), me, "same device key");
        assert!(again.is_verified(&friend));
        again.set_verified(friend, false);
        assert!(!KeyStore::open(Some(dir.clone())).is_verified(&friend), "forgotten");

        let raw = fs::read(dir.join(KEY_FILE)).unwrap();
        let secret = *again.identity().secret_bytes();
        assert!(!raw.windows(32).any(|w| w == secret), "the key is not stored in the clear");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_key_replaces_the_old_and_keeps_the_list() {
        let dir = temp_dir("renew");
        let mut store = KeyStore::open(Some(dir.clone()));
        let friend = Identity::generate().public();
        store.set_verified(friend, true);
        let old = store.public();
        store.renew();
        assert_ne!(store.public(), old);
        let reopened = KeyStore::open(Some(dir.clone()));
        assert_eq!(reopened.public(), store.public());
        assert!(reopened.is_verified(&friend));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_does_not_open_is_set_aside() {
        let dir = temp_dir("bad");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(KEY_FILE), b"copied from another computer").unwrap();
        let store = KeyStore::open(Some(dir.clone()));
        assert!(store.problem.as_deref().is_some_and(|p| p.contains("unreadable")));
        assert!(dir.join("device.key.unreadable").exists(), "kept, not deleted");
        assert_eq!(KeyStore::open(Some(dir.clone())).public(), store.public());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_folder_the_key_lasts_one_run() {
        let a = KeyStore::open(None);
        assert!(a.problem.is_none());
        assert_ne!(a.public(), KeyStore::open(None).public());
    }
}
