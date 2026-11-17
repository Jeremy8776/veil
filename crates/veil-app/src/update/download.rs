//! Fetching release files from GitHub.
//!
//! Purpose: the release's signed checksum list, its signature, and the installer itself, each
//! from a link built here from the repository, the version and a fixed file name; nothing in a
//! GitHub answer chooses what is fetched.
//!
//! Invariants:
//! - HTTPS only, redirects included (GitHub sends release files from its own download host).
//! - Small files are read up to [`SMALL_LIMIT`], installers up to [`FILE_LIMIT`].
//! - An installer is written to `<dest>.part` and hashed as it arrives; only a file whose
//!   SHA-256 matches the signed digest is renamed to `dest`. Anything else is deleted.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{REPO, VERSION, Version};

/// The checksum list and its signature are a few hundred bytes.
const SMALL_LIMIT: u64 = 64 * 1024;
/// Veil's largest installer is tens of megabytes.
const FILE_LIMIT: u64 = 512 * 1024 * 1024;

/// Where `name` from release `version` is downloaded from.
pub fn url(version: Version, name: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}/{name}")
}

fn agent(total: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(total))
        .timeout_connect(Some(Duration::from_secs(10)))
        .https_only(true)
        .http_status_as_error(false)
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let response = agent
        .get(url)
        .header("User-Agent", &format!("veil-app/{VERSION}"))
        .call()
        .map_err(|e| format!("could not reach GitHub ({e})"))?;
    match response.status().as_u16() {
        200 => Ok(response),
        404 => Err(String::from("the release does not have this file")),
        code => Err(format!("GitHub answered {code}")),
    }
}

/// A small release file, whole.
pub fn small(url: &str) -> Result<Vec<u8>, String> {
    let mut response = get(&agent(Duration::from_secs(30)), url)?;
    response
        .body_mut()
        .with_config()
        .limit(SMALL_LIMIT)
        .read_to_vec()
        .map_err(|e| format!("the download failed ({e})"))
}

/// Downloads `url` to `dest`, keeping it only if its SHA-256 is `digest`. `progress` gets
/// the bytes so far and the total, when GitHub gives one.
pub fn file(
    url: &str,
    dest: &Path,
    digest: &[u8; 32],
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut response = get(&agent(Duration::from_secs(30 * 60)), url)?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = response.body_mut().with_config().limit(FILE_LIMIT).reader();
    let part = dest.with_extension("part");
    let written = copy_hashed(&mut reader, &part, progress, total);
    let result = match written {
        Ok(got) if got == *digest => {
            std::fs::rename(&part, dest).map_err(|e| format!("could not keep the download ({e})"))
        }
        Ok(_) => Err(String::from(
            "the download does not match the signed checksum, so it was deleted",
        )),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Copies `reader` into a new file at `path` and returns the SHA-256 of what was written.
fn copy_hashed(
    reader: &mut dyn Read,
    path: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
    total: Option<u64>,
) -> Result<[u8; 32], String> {
    let mut out = std::fs::File::create(path).map_err(|e| format!("could not save ({e})"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("the download failed ({e})"))?;
        let Some(chunk) = buffer.get(..n) else { break };
        if chunk.is_empty() {
            break;
        }
        hash.update(chunk);
        out.write_all(chunk).map_err(|e| format!("could not save ({e})"))?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all().map_err(|e| format!("could not save ({e})"))?;
    Ok(hash.finalize().into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn links_are_built_from_the_version_and_name() {
        assert_eq!(
            url(Version(1, 2, 3), "SHA256SUMS.txt"),
            "https://github.com/Jeremy8776/veil/releases/download/v1.2.3/SHA256SUMS.txt"
        );
    }

    #[test]
    fn a_copy_is_hashed_as_it_is_written() {
        let dir = std::env::temp_dir().join(format!("veil-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file");
        let body = vec![5u8; 200_000];
        let mut seen = 0;
        let got = copy_hashed(&mut body.as_slice(), &path, &mut |done, _| seen = done, None);
        let want: [u8; 32] = Sha256::digest(&body).into();
        assert_eq!(got.unwrap(), want);
        assert_eq!(seen, 200_000);
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
