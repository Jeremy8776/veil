//! The node's identity file: its 32-byte Ed25519 secret, created on first start.
//!
//! Invariants:
//! - An existing file is never overwritten. Creation uses `create_new`, so two nodes started at
//!   once cannot both write a key.
//! - A file of any length other than 32 bytes is refused rather than repaired.
//! - On Unix the file is created readable by its owner only. On Windows it inherits the
//!   folder's permissions, which for a user profile means the owner only.
//! - Secret bytes are wiped from memory after use.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use veil_security::Identity;
use zeroize::Zeroizing;

use crate::NodeError;

const KEY_LEN: usize = 32;

pub fn load_or_create(path: &Path) -> Result<Identity, NodeError> {
    match OpenOptions::new().read(true).open(path) {
        Ok(mut file) => {
            let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_LEN + 1));
            // Read at most one byte too many, so a huge file is not loaded.
            (&mut file)
                .take(KEY_LEN as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| NodeError::Io(path.into(), e))?;
            let secret: Zeroizing<[u8; KEY_LEN]> = Zeroizing::new(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| NodeError::Config(format!("{} is not a 32-byte key", path.display())))?,
            );
            Ok(Identity::from_secret(&secret))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create(path),
        Err(e) => Err(NodeError::Io(path.into(), e)),
    }
}

fn create(path: &Path) -> Result<Identity, NodeError> {
    let identity = Identity::generate();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| NodeError::Io(path.into(), e))?;
    file.write_all(identity.secret_bytes().as_ref())
        .and_then(|()| file.sync_all())
        .map_err(|e| NodeError::Io(path.into(), e))?;
    Ok(identity)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-node-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        dir
    }

    #[test]
    fn creates_once_then_reloads_the_same_key() {
        let path = temp("key");
        let first = load_or_create(&path).unwrap();
        let again = load_or_create(&path).unwrap();
        assert_eq!(first.public(), again.public());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 32);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn wrong_length_file_is_refused_not_overwritten() {
        let path = temp("short");
        std::fs::write(&path, b"not a key").unwrap();
        assert!(load_or_create(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not a key");
        std::fs::remove_file(&path).unwrap();
    }
}
