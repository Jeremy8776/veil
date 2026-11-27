//! Your device key and the keys you have verified, kept between calls.
//!
//! Purpose: one device key for every call, so a safety number checked once still means
//! something next time, and a list of keys you marked as verified after comparing safety
//! numbers. Both live in [`default_dir`], sealed by [`crate::keyseal`]: Windows DPAPI, the
//! Android Keystore, or a wrapping key in the macOS keychain or Linux keyring. They open
//! only for the same user on the same device, and a copied file gives nothing.
//!
//! Invariants:
//! - Verified never admits anyone: it is a label for the host, who still decides.
//! - A saved file is never overwritten unless it opened, was missing, or was moved aside.
//!   A file that does not open (another user, corrupt) is moved to `*.unreadable` (or
//!   `*.unreadable.2` and on, never over an earlier one) and a fresh one is started. A file
//!   that cannot be read or moved (locked, no permission), or whose keychain cannot be asked
//!   (locked keyring, no Secret Service), is left as it is, and that run's key or list is
//!   kept in memory only. The reason is kept for the home screen.
//! - Files are written to a temporary name, flushed to disk and renamed, so a crash never
//!   leaves half a key.
//! - Opened key bytes are wiped once copied into the store.
//! - With no data folder, or on a system [`crate::keyseal`] cannot seal for (see
//!   [`key_dir`]), nothing is saved: the key lasts one run. Other settings still use
//!   [`default_dir`].

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use veil_security::{Identity, PublicId};
use zeroize::Zeroize;

use crate::keyseal::{self, Unseal};

const KEY_FILE: &str = "device.key";
const TRUSTED_FILE: &str = "verified.keys";
const KEY_LABEL: &[u8] = b"veil device key v1";
const TRUSTED_LABEL: &[u8] = b"veil verified keys v1";
/// Keys you may mark as verified. Far more than anyone calls.
pub const MAX_TRUSTED: usize = 10_000;
/// Set-aside copies kept per file before Veil stops moving files aside.
const MAX_ASIDE: u32 = 20;

/// Where Veil keeps its files, if anywhere: `%LOCALAPPDATA%\Veil` on Windows,
/// `~/Library/Application Support/Veil` on macOS, `$XDG_DATA_HOME/veil` (or
/// `~/.local/share/veil`) on Linux.
pub fn default_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    return veil_media::ffi::android::data_dir().ok();
    #[cfg(not(target_os = "android"))]
    dir_for(std::env::consts::OS, |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// Where the device key and verified list are kept: [`default_dir`] where this build can
/// seal them, otherwise nowhere, so the key lasts one run and the home screen shows no
/// save error on every start.
pub fn key_dir() -> Option<PathBuf> {
    default_dir().filter(|_| keyseal::CAN_SEAL)
}

#[cfg(any(not(target_os = "android"), test))]
fn dir_for(os: &str, var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    // The XDG specification says a relative path is invalid and must be ignored.
    let absolute = |name| var(name).filter(|path: &PathBuf| path.has_root());
    match os {
        "windows" => var("LOCALAPPDATA").map(|base| base.join("Veil")),
        "macos" => absolute("HOME").map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("Veil")
        }),
        _ => absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".local").join("share")))
            .map(|base| base.join("veil")),
    }
}

pub struct KeyStore {
    dir: Option<PathBuf>,
    secret: [u8; 32],
    trusted: BTreeSet<PublicId>,
    /// False while a key file Veil could not read is still in place: saving would replace it.
    save_key: bool,
    /// The same, for the verified list.
    save_trusted: bool,
    /// Something that went wrong reading or writing, for the home screen.
    pub problem: Option<String>,
}

/// Why a file was not loaded.
struct Problem {
    message: String,
    /// The file is still where it was, so it must not be written over.
    in_place: bool,
}

impl KeyStore {
    /// Opens the store in `dir`, making a device key on first use.
    pub fn open(dir: Option<PathBuf>) -> Self {
        let mut problem = None;
        let (secret, save_key) = match dir.as_deref().map(load_key) {
            Some(Ok(secret)) => (secret, true),
            None => (None, true),
            Some(Err(p)) => {
                problem = Some(p.message);
                (None, !p.in_place)
            }
        };
        let (trusted, save_trusted) = match dir.as_deref().map(load_trusted) {
            Some(Ok(keys)) => (keys, true),
            None => (BTreeSet::new(), true),
            Some(Err(p)) => {
                problem.get_or_insert(p.message);
                (BTreeSet::new(), !p.in_place)
            }
        };
        let mut store = Self {
            dir,
            secret: [0; 32],
            trusted,
            save_key,
            save_trusted,
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
    /// While an unreadable key file is in place the new key is kept for this run only.
    pub fn renew(&mut self) {
        let fresh = Identity::generate();
        self.secret = *fresh.secret_bytes();
        let Some(dir) = &self.dir else { return };
        if !self.save_key {
            // Keep the first, more specific, reason if there is one.
            self.problem.get_or_insert_with(|| {
                format!(
                    "{KEY_FILE} could not be read or moved, so it was left as it is. \
                     This run uses a key that is not saved."
                )
            });
            return;
        }
        if let Err(e) = save(dir, KEY_FILE, &self.secret, KEY_LABEL) {
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
        let Some(dir) = &self.dir else { return };
        if !changed {
            return;
        }
        if !self.save_trusted {
            self.problem.get_or_insert_with(|| {
                format!(
                    "{TRUSTED_FILE} could not be read or moved, so it was left as it is. \
                     Changes to your verified list last this run only."
                )
            });
            return;
        }
        let bytes: Vec<u8> = self.trusted.iter().flat_map(|k| k.0).collect();
        if let Err(e) = save(dir, TRUSTED_FILE, &bytes, TRUSTED_LABEL) {
            self.problem = Some(format!("Your verified list could not be saved: {e}"));
        }
    }
}

impl Drop for KeyStore {
    fn drop(&mut self) {
        // Best effort: the key also sits in the identity copies handed to calls.
        self.secret.zeroize();
    }
}

/// `Ok(None)` when there is no key yet.
fn load_key(dir: &Path) -> Result<Option<[u8; 32]>, Problem> {
    let Some(mut bytes) = load(dir, KEY_FILE, KEY_LABEL)? else {
        return Ok(None);
    };
    let secret = <[u8; 32]>::try_from(bytes.as_slice());
    bytes.zeroize();
    match secret {
        Ok(secret) => Ok(Some(secret)),
        Err(_) => Err(set_aside(dir, KEY_FILE, "has the wrong length")),
    }
}

fn load_trusted(dir: &Path) -> Result<BTreeSet<PublicId>, Problem> {
    let Some(bytes) = load(dir, TRUSTED_FILE, TRUSTED_LABEL)? else {
        return Ok(BTreeSet::new());
    };
    if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_TRUSTED {
        return Err(set_aside(dir, TRUSTED_FILE, "has the wrong length"));
    }
    Ok(bytes.as_chunks::<32>().0.iter().map(|c| PublicId(*c)).collect())
}

/// Reads and opens `name`. A missing file is `Ok(None)`; one that does not open is moved
/// aside; one that cannot be read is left in place. Both are reported.
fn load(dir: &Path, name: &str, label: &[u8]) -> Result<Option<Vec<u8>>, Problem> {
    let sealed = match fs::read(dir.join(name)) {
        Ok(sealed) => sealed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Problem {
                message: format!(
                    "{name} could not be read ({e}), so it was left as it is and nothing \
                     will replace it this run."
                ),
                in_place: true,
            });
        }
    };
    match keyseal::unseal(&sealed, label) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Unseal::WrongKey) => Err(set_aside(dir, name, keyseal::WRONG_KEY)),
        // It may open once the keychain can be used, so it must not be replaced.
        Err(Unseal::Unavailable(why)) => Err(Problem {
            message: format!(
                "{name} could not be opened because {why}. It was left as it is and \
                 nothing will replace it this run."
            ),
            in_place: true,
        }),
    }
}

/// Moves a bad file out of the way to a name not yet used, keeping it, and says what
/// happened. A rename on Windows replaces its target, so an earlier copy is never reused.
fn set_aside(dir: &Path, name: &str, why: &str) -> Problem {
    let free = (1..=MAX_ASIDE)
        .map(|n| match n {
            1 => dir.join(format!("{name}.unreadable")),
            n => dir.join(format!("{name}.unreadable.{n}")),
        })
        .find(|path| !path.exists());
    match free {
        Some(aside) if fs::rename(dir.join(name), &aside).is_ok() => Problem {
            message: format!(
                "{name} {why}, so a new one was started. It was kept as {}.",
                aside.display()
            ),
            in_place: false,
        },
        _ => Problem {
            message: format!(
                "{name} {why} and could not be moved aside, so it was left as it is and \
                 nothing will replace it this run."
            ),
            in_place: true,
        },
    }
}

fn save(dir: &Path, name: &str, bytes: &[u8], label: &[u8]) -> Result<(), String> {
    let sealed = keyseal::seal(bytes, label)?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("{name}.new"));
    let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(&sealed).map_err(|e| e.to_string())?;
    // On disk before the rename, so a crash never leaves the name on an empty file.
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&temp, dir.join(name)).map_err(|e| e.to_string())
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn data_folder_uses_the_native_user_location() {
        let home = PathBuf::from("/home/a");
        let only = |key: &'static str, value: PathBuf| {
            move |name: &str| (name == key).then(|| value.clone())
        };
        assert_eq!(
            dir_for(
                "windows",
                only("LOCALAPPDATA", PathBuf::from(r"C:\Users\a\AppData\Local"))
            ),
            Some(PathBuf::from(r"C:\Users\a\AppData\Local").join("Veil")),
        );
        assert_eq!(
            dir_for("macos", only("HOME", home.clone())),
            Some(
                home.join("Library")
                    .join("Application Support")
                    .join("Veil")
            ),
        );
        assert_eq!(
            dir_for("linux", only("HOME", home.clone())),
            Some(home.join(".local").join("share").join("veil")),
        );
        assert_eq!(
            dir_for("linux", only("XDG_DATA_HOME", PathBuf::from("/data"))),
            Some(PathBuf::from("/data").join("veil")),
        );
        assert_eq!(
            dir_for("linux", only("XDG_DATA_HOME", PathBuf::from("relative"))),
            None,
            "a relative XDG path is ignored",
        );
        assert_eq!(dir_for("linux", |_| None), None);
    }

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
    fn a_second_bad_file_does_not_replace_the_first() {
        let dir = temp_dir("twice");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(KEY_FILE), b"first").unwrap();
        drop(KeyStore::open(Some(dir.clone())));
        fs::write(dir.join(KEY_FILE), b"second").unwrap();
        drop(KeyStore::open(Some(dir.clone())));
        assert_eq!(fs::read(dir.join("device.key.unreadable")).unwrap(), b"first");
        assert_eq!(fs::read(dir.join("device.key.unreadable.2")).unwrap(), b"second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_written_over() {
        let dir = temp_dir("locked");
        fs::create_dir_all(&dir).unwrap();
        // A directory where the files should be: reading fails with an error that is not
        // "not found", as a locked or forbidden file would.
        fs::create_dir_all(dir.join(KEY_FILE)).unwrap();
        fs::create_dir_all(dir.join(TRUSTED_FILE)).unwrap();
        let mut store = KeyStore::open(Some(dir.clone()));
        assert!(store.problem.as_deref().is_some_and(|p| p.contains("left as it is")));
        store.renew();
        store.set_verified(Identity::generate().public(), true);
        assert!(dir.join(KEY_FILE).is_dir(), "the key's place was not written over");
        assert!(dir.join(TRUSTED_FILE).is_dir(), "the list's place was not written over");
        assert!(!dir.join(format!("{KEY_FILE}.new")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_folder_the_key_lasts_one_run() {
        let a = KeyStore::open(None);
        assert!(a.problem.is_none());
        assert_ne!(a.public(), KeyStore::open(None).public());
    }
}
