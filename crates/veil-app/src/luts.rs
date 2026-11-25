//! Imported LUTs: `.cube` colour looks kept in Veil's own folder, under `luts/`.
//!
//! Purpose: [`Library::import`] checks a chosen file with `veil_media::lut` and, only if it
//! loads, copies it into Veil's folder, so the original is never read again. The effects
//! pickers list [`Library::names`] next to the built-in Filters, and [`Library::load`] builds
//! a look's picture table the first time it is chosen. See ADR 0008.
//!
//! Invariants:
//! - A file that does not parse is not stored, and the reason is shown as plain text.
//! - Stored names are cleaned to letters, digits, spaces and `-_()&.,+`, at most
//!   [`MAX_NAME`] characters, and unique, so a name never reaches outside `luts/`.
//! - At most [`MAX_LUTS`] are kept, so the folder stays small.
//! - Imported LUTs are local data: never sent, and not logged beyond a count.
//! - Each look is built at most once per run and shared with the camera thread by `Arc`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use veil_media::YuvLut;
use veil_media::lut::MAX_FILE;
use veil_media::pick::Picked;

/// Most imported LUTs kept.
pub const MAX_LUTS: usize = 100;
/// Longest stored name, in characters.
pub const MAX_NAME: usize = 40;
const FOLDER: &str = "luts";
const EXTENSION: &str = "cube";

/// A chosen look: its name and its picture table.
#[derive(Clone)]
pub struct ChosenLut {
    pub name: String,
    pub lut: Arc<YuvLut>,
}

impl PartialEq for ChosenLut {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && Arc::ptr_eq(&self.lut, &other.lut)
    }
}

impl std::fmt::Debug for ChosenLut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ChosenLut").field(&self.name).finish()
    }
}

#[derive(Default)]
struct Inner {
    names: Vec<String>,
    built: HashMap<String, Arc<YuvLut>>,
    /// The last import or removal problem, for the effects menu.
    notice: Option<String>,
}

/// The imported LUTs. Shared by `&` from the interface, so it locks inside.
pub struct Library {
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Library {
    /// Lists the LUTs in `base/luts`. Nothing is parsed until a look is chosen.
    pub fn open(base: Option<PathBuf>) -> Self {
        let dir = base.map(|b| b.join(FOLDER));
        let mut names: Vec<String> = dir
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let ok = path.extension().is_some_and(|e| e == EXTENSION);
                ok.then(|| path.file_stem()?.to_str().map(str::to_string))?
            })
            .filter(|name| clean_name(name) == *name)
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.truncate(MAX_LUTS);
        if !names.is_empty() {
            crate::note!("{} imported LUTs", names.len());
        }
        Self {
            dir,
            inner: Mutex::new(Inner {
                names,
                ..Inner::default()
            }),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        crate::media::lock(&self.inner)
    }

    pub fn names(&self) -> Vec<String> {
        self.inner().names.clone()
    }

    /// The last problem, until the next import or removal.
    pub fn notice(&self) -> Option<String> {
        self.inner().notice.clone()
    }

    pub fn set_notice(&self, why: String) {
        self.inner().notice = Some(why);
    }

    pub fn clear_notice(&self) {
        self.inner().notice = None;
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(format!("{name}.{EXTENSION}")))
    }

    /// The look called `name`, built on first use. `None` if it is gone or no longer loads.
    pub fn load(&self, name: &str) -> Option<ChosenLut> {
        if let Some(lut) = self.inner().built.get(name) {
            return Some(ChosenLut {
                name: name.to_string(),
                lut: Arc::clone(lut),
            });
        }
        if !self.inner().names.iter().any(|n| n == name) {
            return None;
        }
        let picked = veil_media::pick::read_capped(&self.path(name)?, MAX_FILE).ok()?;
        let lut = match veil_media::lut::load(&picked.bytes) {
            Ok(lut) => Arc::new(lut),
            Err(why) => {
                self.inner().notice = Some(format!("{name}: {why}"));
                return None;
            }
        };
        self.inner()
            .built
            .insert(name.to_string(), Arc::clone(&lut));
        Some(ChosenLut {
            name: name.to_string(),
            lut,
        })
    }

    /// Checks `picked` and stores a copy. Returns the new look, or a plain reason.
    pub fn import(&self, picked: &Picked) -> Result<ChosenLut, String> {
        let result = self.try_import(picked);
        self.inner().notice = result.as_ref().err().cloned();
        result
    }

    fn try_import(&self, picked: &Picked) -> Result<ChosenLut, String> {
        let dir = self
            .dir
            .clone()
            .ok_or("Veil has no folder to keep LUTs in on this device.")?;
        if self.inner().names.len() >= MAX_LUTS {
            return Err(format!(
                "You have {MAX_LUTS} LUTs. Remove one to import another."
            ));
        }
        let lut = veil_media::lut::load(&picked.bytes).map_err(|e| e.to_string())?;
        let name = unique(&clean_name(stem(&picked.name)), &self.inner().names);
        crate::json_file::write_bytes(&dir, &format!("{name}.{EXTENSION}"), &picked.bytes)
            .map_err(|_| String::from("The LUT could not be saved."))?;
        let lut = Arc::new(lut);
        let mut inner = self.inner();
        inner.names.push(name.clone());
        inner.names.sort_by_key(|n| n.to_lowercase());
        inner.built.insert(name.clone(), Arc::clone(&lut));
        Ok(ChosenLut { name, lut })
    }

    /// Deletes the stored copy of `name`.
    pub fn remove(&self, name: &str) {
        let gone = self
            .path(name)
            .is_some_and(|p| std::fs::remove_file(p).is_ok());
        let mut inner = self.inner();
        inner.notice = (!gone).then(|| format!("{name} could not be removed."));
        if gone {
            inner.names.retain(|n| n != name);
            inner.built.remove(name);
        }
    }
}

/// The file name without a `.cube` ending, in any case.
fn stem(file: &str) -> &str {
    match file.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case(EXTENSION) => stem,
        _ => file,
    }
}

/// A safe, short display and file name. See the module invariants.
pub fn clean_name(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || " -_()&.,+".contains(*c))
        .take(MAX_NAME)
        .collect();
    let name = kept.trim().trim_matches('.').trim();
    if name.is_empty() {
        String::from("LUT")
    } else {
        name.to_string()
    }
}

/// `name`, or `name 2`, `name 3`... if it is taken (ignoring case).
fn unique(name: &str, taken: &[String]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if free(name) {
        return name.to_string();
    }
    (2..)
        .map(|i| format!("{name} {i}"))
        .find(|n| free(n))
        .unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const IDENTITY: &str =
        "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-luts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn picked(name: &str, text: &str) -> Picked {
        Picked {
            name: name.to_string(),
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn names_are_cleaned_and_unique() {
        assert_eq!(clean_name("../../etc/passwd"), "etcpasswd");
        assert_eq!(clean_name("C:\\x\\Teal & Orange"), "CxTeal & Orange");
        assert_eq!(clean_name("  ...  "), "LUT");
        assert_eq!(clean_name(&"a".repeat(90)).len(), MAX_NAME);
        assert_eq!(stem("Warm.CUBE"), "Warm");
        assert_eq!(stem("Warm.txt"), "Warm.txt");
        let taken = vec![String::from("Warm"), String::from("warm 2")];
        assert_eq!(unique("Warm", &taken), "Warm 3");
        assert_eq!(unique("Cool", &taken), "Cool");
    }

    #[test]
    fn import_stores_a_copy_and_lists_it_after_a_restart() {
        let base = temp_dir("import");
        let library = Library::open(Some(base.clone()));
        let look = library.import(&picked("Film Look.cube", IDENTITY)).unwrap();
        assert_eq!(look.name, "Film Look");
        assert!(base.join("luts/Film Look.cube").exists());
        let again = library.import(&picked("film look.cube", IDENTITY)).unwrap();
        assert_eq!(again.name, "film look 2");
        let reopened = Library::open(Some(base.clone()));
        assert_eq!(reopened.names(), ["Film Look", "film look 2"]);
        assert!(reopened.load("Film Look").is_some());
        assert!(reopened.load("Missing").is_none());
        reopened.remove("Film Look");
        assert_eq!(reopened.names(), ["film look 2"]);
        assert!(!base.join("luts/Film Look.cube").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_bad_file_is_refused_and_not_stored() {
        let base = temp_dir("bad");
        let library = Library::open(Some(base.clone()));
        let why = library
            .import(&picked("Bad.cube", "LUT_3D_SIZE 2\n0 0 0\n"))
            .err();
        assert_eq!(
            why.as_deref(),
            Some("The number of colours does not match the LUT size.")
        );
        assert_eq!(library.notice(), why);
        assert!(library.names().is_empty());
        assert!(!base.join("luts/Bad.cube").exists());
        let none = Library::open(None);
        assert!(
            none.import(&picked("A.cube", IDENTITY)).is_err(),
            "no folder"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
