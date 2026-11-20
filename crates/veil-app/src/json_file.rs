//! Small JSON files in Veil's folder: read with a size cap, written so a crash never leaves
//! half a file.
//!
//! Purpose: `connection.json` and `preferences.json` share these two functions, so both read
//! and write the same safe way.
//!
//! Invariants:
//! - Reading stops after `max` bytes; a larger file is an error, not read whole.
//! - A missing file is `Ok(None)`, not an error.
//! - Writes go to `<name>.new`, are flushed to disk and renamed over `name`.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads `dir/name`. `Ok(None)` when there is no file yet.
pub fn load<T: DeserializeOwned>(dir: &Path, name: &str, max: u64) -> Result<Option<T>, String> {
    let file = match fs::File::open(dir.join(name)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(max + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > max {
        return Err(String::from("the file is too large"));
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("not valid settings: {e}"))
}

/// Writes `value` to `dir/name`, creating `dir` if needed.
pub fn write<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("{name}.new"));
    let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    // On disk before the rename, so a crash never leaves the name on an empty file.
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&temp, dir.join(name)).map_err(|e| e.to_string())
}
