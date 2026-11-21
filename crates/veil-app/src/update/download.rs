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

use super::{NetworkGate, REPO, VERSION, Version};

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
        // Follow explicitly, checking the shared policy before every redirected request.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

pub(super) fn get(
    url: &str,
    total: Duration,
    network: &NetworkGate,
    api: bool,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let agent = agent(total);
    let started = std::time::Instant::now();
    request_with(url, network, |url| {
        let remaining = total
            .checked_sub(started.elapsed())
            .ok_or("the download timed out")?;
        let mut request = agent
            .get(url)
            .header("User-Agent", &format!("veil-app/{VERSION}"));
        if api {
            request = request.header("Accept", "application/vnd.github+json");
        }
        request
            .config()
            .timeout_global(Some(remaining))
            .build()
            .call()
            .map_err(|e| format!("could not reach GitHub ({e})"))
    })
}

fn request_with(
    url: &str,
    network: &NetworkGate,
    mut request: impl FnMut(&str) -> Result<ureq::http::Response<ureq::Body>, String>,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut url = url.to_owned();
    for _ in 0..=10 {
        network.check()?;
        let current: ureq::http::Uri = url.parse().map_err(|_| "invalid release URL")?;
        if current.scheme_str() != Some("https") || current.authority().is_none() {
            return Err(String::from("the release URL must use HTTPS"));
        }
        let response = request(&url)?;
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or("the redirect has no valid location")?;
        // GitHub uses absolute download links. Also accept origin-relative locations;
        // unsupported relative forms fail closed rather than bypassing policy checks.
        url = if location.starts_with('/') && !location.starts_with("//") {
            format!(
                "https://{}{location}",
                current.authority().ok_or("invalid release URL")?
            )
        } else {
            location.to_owned()
        };
    }
    Err(String::from("too many release redirects"))
}

fn successful(
    response: ureq::http::Response<ureq::Body>,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    match response.status().as_u16() {
        200 => Ok(response),
        404 => Err(String::from("the release does not have this file")),
        code => Err(format!("GitHub answered {code}")),
    }
}

/// A small release file, whole.
pub fn small(url: &str, network: &NetworkGate) -> Result<Vec<u8>, String> {
    let mut response = successful(get(url, Duration::from_secs(30), network, false)?)?;
    network.check()?;
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
    network: &NetworkGate,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut response = successful(get(url, Duration::from_secs(30 * 60), network, false)?)?;
    network.check()?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = response.body_mut().with_config().limit(FILE_LIMIT).reader();
    let part = dest.with_extension("part");
    let written = copy_hashed(&mut reader, &part, progress, total, network);
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
    network: &NetworkGate,
) -> Result<[u8; 32], String> {
    let mut out = std::fs::File::create(path).map_err(|e| format!("could not save ({e})"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        network.check()?;
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("the download failed ({e})"))?;
        let Some(chunk) = buffer.get(..n) else { break };
        if chunk.is_empty() {
            break;
        }
        hash.update(chunk);
        out.write_all(chunk)
            .map_err(|e| format!("could not save ({e})"))?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all()
        .map_err(|e| format!("could not save ({e})"))?;
    Ok(hash.finalize().into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn switching_to_local_only_stops_the_next_redirect_request() {
        let network = NetworkGate::new(true);
        let mut requests = 0;
        let result = request_with("https://github.com/release", &network, |_| {
            requests += 1;
            network.set(false);
            Ok(ureq::http::Response::builder()
                .status(302)
                .header(
                    "location",
                    "https://release-assets.githubusercontent.com/file",
                )
                .body(ureq::Body::builder().data(Vec::new()))
                .unwrap())
        });
        assert!(result.unwrap_err().contains("Local only"));
        assert_eq!(requests, 1, "a later HTTP request must not start");
    }

    #[test]
    fn a_disabled_worker_never_sends_its_first_request() {
        let mut requests = 0;
        let result = request_with(
            "https://github.com/release",
            &NetworkGate::new(false),
            |_| {
                requests += 1;
                Err(String::from("an unexpected HTTP request was attempted"))
            },
        );
        assert!(result.unwrap_err().contains("Local only"));
        assert_eq!(requests, 0);
    }

    #[test]
    fn redirects_cannot_downgrade_to_http() {
        let mut requests = 0;
        let result = request_with(
            "https://github.com/release",
            &NetworkGate::new(true),
            |_| {
                requests += 1;
                Ok(ureq::http::Response::builder()
                    .status(302)
                    .header("location", "http://example.com/file")
                    .body(ureq::Body::builder().data(Vec::new()))
                    .unwrap())
            },
        );
        assert!(result.unwrap_err().contains("HTTPS"));
        assert_eq!(requests, 1);
    }

    #[test]
    fn a_download_stops_reading_after_local_only_is_enabled() {
        let dir = std::env::temp_dir().join(format!("veil-download-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.part");
        let body = vec![5u8; 200_000];
        let network = NetworkGate::new(true);
        let result = copy_hashed(
            &mut body.as_slice(),
            &path,
            &mut |_, _| network.set(false),
            None,
            &network,
        );
        assert!(result.unwrap_err().contains("Local only"));
        assert!(std::fs::metadata(&path).unwrap().len() < body.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let got = copy_hashed(
            &mut body.as_slice(),
            &path,
            &mut |done, _| seen = done,
            None,
            &NetworkGate::new(true),
        );
        let want: [u8; 32] = Sha256::digest(&body).into();
        assert_eq!(got.unwrap(), want);
        assert_eq!(seen, 200_000);
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
