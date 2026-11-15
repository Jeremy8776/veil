//! Builds libopus from the sources in `vendor/opus` with the `cc` crate, so no cmake is needed.
//!
//! Invariants:
//! - The file lists come from libopus's own `*_sources.mk`, so updating libopus means
//!   replacing `vendor/opus` (see `vendor/opus/VENDORED.md`), not editing this file.
//! - Float build, no deep-learning features (DRED, OSCE, deep PLC): `vendor/opus` has no `dnn`
//!   folder, and none of their `ENABLE_*` defines are set.
//! - `ENABLE_HARDENING` keeps libopus's cheap run-time checks on in release builds.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::{env, fs};

/// The `*_sources.mk` variables that make up a float build without SIMD.
const LISTS: [(&str, &str); 5] = [
    ("celt_sources.mk", "CELT_SOURCES"),
    ("silk_sources.mk", "SILK_SOURCES"),
    ("silk_sources.mk", "SILK_SOURCES_FLOAT"),
    ("opus_sources.mk", "OPUS_SOURCES"),
    ("opus_sources.mk", "OPUS_SOURCES_FLOAT"),
];

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?)
        .join("vendor")
        .join("opus");
    println!("cargo:rerun-if-changed=vendor/opus");

    let version = fs::read_to_string(root.join("package_version"))?;
    let version = version
        .trim()
        .strip_prefix("PACKAGE_VERSION=")
        .ok_or("vendor/opus/package_version has no PACKAGE_VERSION")?
        .trim_matches('"')
        .to_owned();

    let mut build = cc::Build::new();
    for list in LISTS {
        for file in sources(&root, list.0, list.1)? {
            build.file(root.join(file));
        }
    }
    for dir in ["include", "celt", "silk", "silk/float", "."] {
        build.include(root.join(dir));
    }
    build
        .define("OPUS_BUILD", None)
        .define("ENABLE_HARDENING", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        .define("PACKAGE_VERSION", format!("\"{version}\"").as_str())
        // libopus is slow unoptimised, and a debug build of the app still has to keep up with
        // live audio.
        .opt_level(2)
        .warnings(false);
    if env::var("CARGO_CFG_TARGET_ENV")? == "msvc" {
        // MSVC has no variable-length arrays.
        build.define("USE_ALLOCA", None);
    } else {
        build.define("VAR_ARRAYS", None);
    }
    // shortcut: no SSE, AVX or NEON paths and no run-time CPU detection. Plain C is fast
    // enough for a few voice streams; add the x86 and arm lists when a profile shows a need.
    build.compile("opus");

    let os = env::var("CARGO_CFG_TARGET_OS")?;
    if os != "windows" && os != "macos" && os != "ios" {
        println!("cargo:rustc-link-lib=m");
    }
    Ok(())
}

/// The `.c` files that `variable` lists in `vendor/opus/<makefile>`.
fn sources(root: &Path, makefile: &str, variable: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let text = fs::read_to_string(root.join(makefile))?;
    let mut lines = text.lines();
    lines
        .by_ref()
        .find(|line| {
            line.split_once('=')
                .is_some_and(|(name, _)| name.trim() == variable)
        })
        .ok_or_else(|| format!("{makefile} has no {variable}"))?;
    let mut files = Vec::new();
    for line in lines {
        let entry = line.trim().trim_end_matches('\\').trim();
        if entry.is_empty() {
            break;
        }
        files.push(entry.to_owned());
    }
    if files.is_empty() {
        return Err(format!("{makefile} lists no files in {variable}").into());
    }
    Ok(files)
}
