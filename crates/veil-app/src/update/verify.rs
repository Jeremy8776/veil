//! Proving a release file came from Veil's release key.
//!
//! Purpose: every release carries `SHA256SUMS.txt` (one `hash  file name` line per file) and
//! `SHA256SUMS.txt.sig`, an Ed25519 signature over those exact bytes, made in the release
//! workflow with Veil's release key. The public half of that key is built into the app
//! ([`release_key`], from `crates/veil-app/release-key.pub`). A file is installed only when
//! its SHA-256 matches the line for its name in a list that key signed.
//!
//! Invariants:
//! - No key built in, no installing: [`release_key`] is `None` and the app only links to the
//!   release page.
//! - The signature is checked with `verify_strict` (no weak keys, no malleable signatures)
//!   before a single line of the list is read.
//! - The list names each file with its version (`veil-1.2.3-...`), so an older signed list
//!   cannot vouch for a file under a newer name.
//! - The key signs nothing else, so the signed bytes need no context prefix.

use ed25519_dalek::{Signature, VerifyingKey};

/// The release key's public half, as 64 hex digits, or nothing while no key is set.
/// `tools/release-key.sh` writes it.
const RELEASE_KEY_HEX: &str = include_str!("../../release-key.pub");

/// Veil's release key, if this build has one.
pub fn release_key() -> Option<[u8; 32]> {
    parse_key(RELEASE_KEY_HEX)
}

/// Reads 64 hex digits (surrounding white space allowed) as a key.
fn parse_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(key)
}

/// The SHA-256 the signed list gives for `name`, once the signature over `sums` checks out.
pub fn signed_digest(
    sums: &[u8],
    signature: &[u8],
    key: &[u8; 32],
    name: &str,
) -> Result<[u8; 32], String> {
    let key = VerifyingKey::from_bytes(key).map_err(|_| "the release key is not valid")?;
    let signature: &[u8; 64] = signature
        .try_into()
        .map_err(|_| "the release signature is the wrong size")?;
    key.verify_strict(sums, &Signature::from_bytes(signature))
        .map_err(|_| "the release signature does not match Veil's release key")?;
    let text = std::str::from_utf8(sums).map_err(|_| "the checksum list is not text")?;
    let mut found = None;
    for line in text.lines() {
        if let Some((digest, file)) = parse_line(line)
            && file == name
        {
            if found.is_some() {
                return Err(format!("the checksum list names {name} twice"));
            }
            found = Some(digest);
        }
    }
    found.ok_or_else(|| format!("the signed checksum list does not name {name}"))
}

/// One `sha256sum` line: 64 hex digits, then two spaces (or a space and `*`), then the name.
fn parse_line(line: &str) -> Option<([u8; 32], &str)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (hex, rest) = (line.get(..64)?, line.get(64..)?);
    let name = rest.strip_prefix("  ").or_else(|| rest.strip_prefix(" *"))?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((parse_key(hex)?, name))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const NAME: &str = "veil-1.2.3-windows-x64.msi";

    fn signed(text: &str) -> (Vec<u8>, Vec<u8>, [u8; 32]) {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let signature = signer.sign(text.as_bytes()).to_bytes().to_vec();
        (text.as_bytes().to_vec(), signature, signer.verifying_key().to_bytes())
    }

    fn list() -> String {
        format!(
            "{}  veil-1.2.3-linux-x64.AppImage\n{}  {NAME}\n",
            "ab".repeat(32),
            "0F".repeat(32)
        )
    }

    #[test]
    fn a_signed_list_gives_the_files_digest() {
        let (sums, signature, key) = signed(&list());
        assert_eq!(signed_digest(&sums, &signature, &key, NAME).unwrap(), [0x0f; 32]);
    }

    #[test]
    fn a_changed_list_or_another_key_is_refused() {
        let (mut sums, signature, key) = signed(&list());
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(signed_digest(&sums, &signature, &other, NAME).is_err(), "another key");
        sums[0] = b'c';
        assert!(signed_digest(&sums, &signature, &key, NAME).is_err(), "changed list");
        assert!(signed_digest(&sums, &signature[..63], &key, NAME).is_err(), "short signature");
    }

    #[test]
    fn only_an_exact_name_matches_once() {
        let (sums, signature, key) = signed(&list());
        for name in ["veil-1.2.3-windows-x64", "veil-1.2.4-windows-x64.msi", "", " veil"] {
            assert!(signed_digest(&sums, &signature, &key, name).is_err(), "{name:?}");
        }
        let twice = format!("{}{}", list(), list());
        let (sums, signature, key) = signed(&twice);
        assert!(signed_digest(&sums, &signature, &key, NAME).is_err(), "named twice");
    }

    #[test]
    fn lines_are_read_as_sha256sum_writes_them() {
        let hash = "12".repeat(32);
        assert_eq!(parse_line(&format!("{hash}  a b.zip")).unwrap().1, "a b.zip");
        assert_eq!(parse_line(&format!("{hash} *a.zip\r")).unwrap().1, "a.zip");
        assert!(parse_line(&format!("{hash} a.zip")).is_none(), "one space");
        assert!(parse_line(&format!("{}  a.zip", "zz".repeat(32))).is_none(), "not hex");
        assert!(parse_line("short  a.zip").is_none());
    }

    /// What the release workflow does, with OpenSSL: `cargo test -j 8 -p veil-app
    /// openssl -- --ignored`. Needs OpenSSL 3 on PATH (Git Bash has it).
    #[test]
    #[ignore = "runs openssl"]
    fn openssl_signatures_verify() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("veil-openssl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (key, sums, sig) = (dir.join("key.pem"), dir.join("SHA256SUMS.txt"), dir.join("sig"));
        std::fs::write(&sums, list()).unwrap();
        let openssl = |args: &[&std::ffi::OsStr]| {
            let out = Command::new("openssl").args(args).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            out.stdout
        };
        openssl(&["genpkey".as_ref(), "-algorithm".as_ref(), "ed25519".as_ref(), "-out".as_ref(), key.as_os_str()]);
        openssl(&[
            "pkeyutl".as_ref(), "-sign".as_ref(), "-inkey".as_ref(), key.as_os_str(),
            "-rawin".as_ref(), "-in".as_ref(), sums.as_os_str(), "-out".as_ref(), sig.as_os_str(),
        ]);
        let der = openssl(&["pkey".as_ref(), "-in".as_ref(), key.as_os_str(), "-pubout".as_ref(), "-outform".as_ref(), "DER".as_ref()]);
        let public: [u8; 32] = der[der.len() - 32..].try_into().unwrap();
        let (sums, sig) = (std::fs::read(&sums).unwrap(), std::fs::read(&sig).unwrap());
        assert_eq!(signed_digest(&sums, &sig, &public, NAME).unwrap(), [0x0f; 32]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_built_in_key_is_empty_or_valid() {
        let text = RELEASE_KEY_HEX.trim();
        assert!(text.is_empty() || release_key().is_some(), "release-key.pub: {text:?}");
        if let Some(key) = release_key() {
            assert!(VerifyingKey::from_bytes(&key).is_ok());
        }
        assert_eq!(parse_key(&"aB".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_key(&"ab".repeat(31)), None);
        assert_eq!(parse_key(&format!("{}zz", "ab".repeat(31))), None);
    }
}
