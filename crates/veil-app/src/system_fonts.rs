//! Finding a UI font on Linux, where each distribution keeps fonts in its own folders.
//!
//! Purpose: Fedora, for one, has neither of the fixed paths in `theme.rs`, and a missing UI font
//! stops Veil from opening. This searches the standard font folders for a common sans font.
//!
//! Invariants:
//! - Runs only on Linux and similar systems, and only when the fixed paths found nothing.
//! - Reads only known file names under absolute font folders: a relative `XDG_*` entry is
//!   ignored, as the XDG specification requires, so the working directory is never searched.
//! - The search is bounded in depth and entries, so a link loop or huge folder cannot hang
//!   start-up.
//! - A file must start like a font: egui panics on data it cannot parse.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Regular sans fonts egui can draw, best first. Collections (`.ttc`) are left out.
const UI_FONT_FILES: &[&str] = &[
    "DejaVuSans.ttf",
    "NotoSans-Regular.ttf",
    "NotoSans[wght].ttf",
    "AdwaitaSans-Regular.ttf",
    "LiberationSans-Regular.ttf",
];
const MAX_DEPTH: usize = 6;
const MAX_ENTRIES: usize = 50_000;

/// The contents of the best UI font in the system's font folders, if there is one.
pub fn find_ui_font() -> Option<Vec<u8>> {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return None;
    }
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let roots = font_roots(var("XDG_DATA_DIRS"), var("XDG_DATA_HOME"), var("HOME"));
    candidates(&roots).iter().find_map(|path| read_font(path))
}

/// System font folders first, then this user's.
fn font_roots(
    data_dirs: Option<OsString>,
    data_home: Option<OsString>,
    home: Option<OsString>,
) -> Vec<PathBuf> {
    let home = home.map(PathBuf::from).filter(|path| path.has_root());
    let mut bases: Vec<PathBuf> = data_dirs
        .map(|dirs| std::env::split_paths(&dirs).collect())
        .unwrap_or_default();
    // Always included: a launcher can replace XDG_DATA_DIRS and leave these out.
    bases.extend(["/usr/local/share", "/usr/share"].map(PathBuf::from));
    let data_home = data_home.map(PathBuf::from).filter(|path| path.has_root());
    bases.extend(data_home.or_else(|| home.as_ref().map(|h| h.join(".local").join("share"))));
    let mut roots: Vec<PathBuf> = bases
        .into_iter()
        .filter(|base| base.has_root())
        .map(|base| base.join("fonts"))
        .collect();
    roots.extend(home.map(|h| h.join(".fonts")));
    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

/// Every known font under `roots`, best first.
fn candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut entries_left = MAX_ENTRIES;
    for root in roots {
        walk(root, 0, &mut entries_left, &mut found);
    }
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

fn walk(dir: &Path, depth: usize, left: &mut usize, found: &mut Vec<(usize, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *left == 0 {
            return;
        }
        *left -= 1;
        let path = entry.path();
        let name = entry.file_name();
        let rank = UI_FONT_FILES.iter().position(|known| name.to_str() == Some(known));
        if let Some(rank) = rank {
            found.push((rank, path));
        } else if depth < MAX_DEPTH && path.is_dir() {
            walk(&path, depth + 1, left, found);
        }
    }
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok().filter(|bytes| looks_like_font(bytes))
}

/// The four bytes that open a TrueType or OpenType file.
fn looks_like_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0, 1, 0, 0] | [b'O', b'T', b'T', b'O'] | [b't', b'r', b'u', b'e'])
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FONT: &[u8] = &[0, 1, 0, 0, 9];

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-fonts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn finds_a_font_in_a_distribution_folder_and_prefers_the_best() {
        let dir = temp("rank");
        let noto = dir.join("google-noto-vf").join("NotoSans[wght].ttf");
        let dejavu = dir.join("dejavu-sans-fonts").join("DejaVuSans.ttf");
        put(&noto, FONT);
        put(&dejavu, FONT);
        put(&dir.join("other").join("Unknown.ttf"), FONT);
        assert_eq!(candidates(std::slice::from_ref(&dir)), vec![dejavu, noto]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_past_the_depth_limit_are_not_searched() {
        let dir = temp("depth");
        let mut deep = dir.clone();
        for _ in 0..=MAX_DEPTH {
            deep = deep.join("d");
        }
        put(&deep.join("DejaVuSans.ttf"), FONT);
        assert!(candidates(std::slice::from_ref(&dir)).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_a_font_is_refused() {
        let dir = temp("bad");
        let bad = dir.join("DejaVuSans.ttf");
        put(&bad, b"<html>");
        assert_eq!(read_font(&bad), None);
        put(&bad, FONT);
        assert_eq!(read_font(&bad).as_deref(), Some(FONT));
        assert!(looks_like_font(b"OTTO") && looks_like_font(b"true"));
        assert!(!looks_like_font(b"ttcf"), "collections are not read");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn roots_put_the_system_first_and_ignore_relative_paths() {
        let dirs = std::env::join_paths(["/opt/share", "relative", "/usr/share"]).unwrap();
        let roots = font_roots(Some(dirs), Some("also-relative".into()), Some("/home/a".into()));
        let expected: Vec<PathBuf> = [
            Path::new("/opt/share/fonts").to_path_buf(),
            Path::new("/usr/share/fonts").to_path_buf(),
            Path::new("/usr/local/share/fonts").to_path_buf(),
            Path::new("/home/a/.local/share").join("fonts"),
            Path::new("/home/a").join(".fonts"),
        ]
        .into_iter()
        .map(|path| path.components().collect())
        .collect();
        let roots: Vec<PathBuf> = roots.iter().map(|path| path.components().collect()).collect();
        assert_eq!(roots, expected);
    }
}
