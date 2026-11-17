//! Your connection settings: servers of your own, a fixed port, and whether Veil may open
//! that port on your router, kept between runs.
//!
//! Purpose: Settings > Connection edits a [`Settings`] (the text of each field), checks it
//! with the same parsers as the environment variables (`engine::plan`), and saves it to
//! `connection.json` in Veil's folder. A call starts from the saved settings, and each
//! environment variable that is set replaces its field (`NetConfig::with_env`).
//!
//! Invariants:
//! - Only settings that pass [`Settings::check`] are saved, so a saved file always parses.
//! - The file holds public server keys and addresses, nothing secret, so it is plain JSON.
//! - A file that does not load (corrupt, edited by hand, too large) is reported and left in
//!   place. Saving over it first moves it to `connection.json.bak`, so it is never lost
//!   silently.
//! - Writes go to a temporary name, are flushed to disk and renamed, so a crash never leaves
//!   half a file.
//! - Reading stops at `MAX_FILE` bytes; each field is at most `MAX_FIELD` characters.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::plan::{self, NetConfig};

const FILE: &str = "connection.json";
const BACKUP: &str = "connection.json.bak";
/// Far more than four fields need.
const MAX_FILE: u64 = 16 * 1024;
/// Longest text accepted in one field.
pub const MAX_FIELD: usize = 512;

/// One field of the settings, for errors and environment overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Rendezvous,
    Forwarder,
    Advertise,
    Port,
}

impl Field {
    pub const ALL: [Self; 4] = [
        Self::Rendezvous,
        Self::Forwarder,
        Self::Advertise,
        Self::Port,
    ];

    /// The environment variable that overrides this field.
    pub fn env(self) -> &'static str {
        match self {
            Self::Rendezvous => plan::ENV_RENDEZVOUS,
            Self::Forwarder => plan::ENV_FORWARDER,
            Self::Advertise => plan::ENV_ADVERTISE,
            Self::Port => plan::ENV_PORT,
        }
    }
}

/// The text of each field, as typed. Blank means Veil's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub rendezvous: String,
    pub forwarder: String,
    pub advertise: String,
    pub port: String,
    /// Ask the router to open the call port while hosting. Off until the person says yes.
    pub open_port: bool,
    /// The question before the first hosted call has been answered, either way.
    pub asked_open_port: bool,
}

impl Settings {
    pub fn field_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Rendezvous => &mut self.rendezvous,
            Field::Forwarder => &mut self.forwarder,
            Field::Advertise => &mut self.advertise,
            Field::Port => &mut self.port,
        }
    }

    fn field(&self, field: Field) -> &str {
        match field {
            Field::Rendezvous => &self.rendezvous,
            Field::Forwarder => &self.forwarder,
            Field::Advertise => &self.advertise,
            Field::Port => &self.port,
        }
    }

    /// The network settings these fields give, or what is wrong with each bad field.
    pub fn check(&self) -> Result<NetConfig, Vec<(Field, String)>> {
        let mut problems = Vec::new();
        let mut config = NetConfig {
            open_port: self.open_port,
            ..NetConfig::default()
        };
        for field in Field::ALL {
            let text = self.field(field);
            if text.chars().count() > MAX_FIELD {
                problems.push((field, format!("At most {MAX_FIELD} characters.")));
                continue;
            }
            let result = match field {
                Field::Rendezvous => plan::parse_node(text).map(|n| config.rendezvous = n),
                Field::Forwarder => plan::parse_node(text).map(|n| config.forwarder = n),
                Field::Advertise => plan::parse_advertise(text).map(|a| config.advertise = a),
                Field::Port => plan::parse_port(text).map(|p| config.port = p),
            };
            if let Err(e) = result {
                problems.push((field, e));
            }
        }
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(problems)
        }
    }

    /// The fields an environment variable currently replaces.
    pub fn overridden(get: impl Fn(&str) -> Option<String>) -> Vec<Field> {
        Field::ALL
            .into_iter()
            .filter(|f| get(f.env()).is_some_and(|v| !v.trim().is_empty()))
            .collect()
    }
}

/// The saved settings and where they live.
pub struct Store {
    dir: Option<PathBuf>,
    pub saved: Settings,
    /// The file on disk did not load, so saving must first move it aside.
    bad_file: bool,
    /// Something that went wrong reading or writing, for the settings screen.
    pub problem: Option<String>,
}

impl Store {
    /// Opens the settings in `dir`. Missing is the defaults; a bad file is reported.
    pub fn open(dir: Option<PathBuf>) -> Self {
        let mut store = Self {
            dir,
            saved: Settings::default(),
            bad_file: false,
            problem: None,
        };
        let Some(dir) = store.dir.clone() else {
            return store;
        };
        match load(&dir) {
            Ok(Some(settings)) => {
                if settings.check().is_err() {
                    store.problem = Some(String::from(
                        "A saved connection setting is not valid, so Veil uses its defaults \
                         until you fix it and save.",
                    ));
                }
                store.saved = settings;
            }
            Ok(None) => {}
            Err(why) => {
                crate::note!("connection settings not loaded: {why}");
                store.problem = Some(format!(
                    "Your saved connection settings could not be read ({why}). Veil uses \
                     its defaults. Saving keeps the old file as {BACKUP}."
                ));
                store.bad_file = true;
            }
        }
        store
    }

    /// The settings a call starts from: the saved ones, then the environment.
    pub fn config(&self) -> NetConfig {
        // A saved file always parses, but one edited by hand may not: use the defaults then.
        let saved = self.saved.check().unwrap_or_else(|problems| {
            crate::note!("saved connection settings ignored: {problems:?}");
            NetConfig::default()
        });
        saved.with_env(|name| std::env::var(name).ok())
    }

    /// Checks and saves `settings`. Nothing is saved unless every field is good.
    pub fn save(&mut self, settings: Settings) -> Result<(), Vec<(Field, String)>> {
        settings.check()?;
        let Some(dir) = self.dir.clone() else {
            self.saved = settings;
            self.problem = Some(String::from(
                "Veil has no folder to save in here, so these settings last this run only.",
            ));
            return Ok(());
        };
        if self.bad_file {
            // Kept, not deleted: it may hold values someone typed by hand.
            if let Err(e) = fs::rename(dir.join(FILE), dir.join(BACKUP))
                && e.kind() != std::io::ErrorKind::NotFound
            {
                self.saved = settings;
                self.problem = Some(format!(
                    "The old settings file could not be moved aside ({e}), so it was left as \
                     it is. These settings last this run only."
                ));
                return Ok(());
            }
            self.bad_file = false;
        }
        match write(&dir, &settings) {
            Ok(()) => self.problem = None,
            Err(e) => {
                self.problem = Some(format!(
                    "Your settings could not be saved ({e}). They last this run only."
                ));
            }
        }
        self.saved = settings;
        Ok(())
    }
}

/// `Ok(None)` when there is no file yet.
fn load(dir: &Path) -> Result<Option<Settings>, String> {
    let file = match fs::File::open(dir.join(FILE)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FILE {
        return Err(String::from("the file is too large"));
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("not valid settings: {e}"))
}

fn write(dir: &Path, settings: &Settings) -> Result<(), String> {
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("{FILE}.new"));
    let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    // On disk before the rename, so a crash never leaves the name on an empty file.
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&temp, dir.join(FILE)).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_net::NodeAddr;
    use veil_security::Identity;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("veil-connection-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn node(addr: &str) -> String {
        NodeAddr {
            key: Identity::generate().public(),
            addr: addr.parse().unwrap(),
        }
        .to_string()
    }

    fn good() -> Settings {
        Settings {
            rendezvous: node("203.0.113.5:4433"),
            forwarder: node("203.0.113.5:4434"),
            advertise: String::from("198.51.100.2:7710"),
            port: String::from("7710"),
            open_port: true,
            asked_open_port: true,
        }
    }

    #[test]
    fn the_router_choice_is_kept_and_off_by_default() {
        assert!(!Settings::default().check().unwrap().open_port);
        assert!(good().check().unwrap().open_port);
        let dir = temp_dir("router");
        fs::create_dir_all(&dir).unwrap();
        // A file saved before the choice existed.
        fs::write(dir.join(FILE), br#"{"port":"7710"}"#).unwrap();
        let mut store = Store::open(Some(dir.clone()));
        assert!(!store.saved.open_port && !store.saved.asked_open_port);
        store.save(good()).unwrap();
        let again = Store::open(Some(dir.clone()));
        assert!(again.saved.open_port && again.saved.asked_open_port);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn good_settings_check_and_blank_is_the_default() {
        let config = good().check().unwrap();
        assert!(config.rendezvous.is_some() && config.forwarder.is_some());
        assert_eq!(config.port, 7710);
        assert_eq!(Settings::default().check().unwrap(), NetConfig::default());
    }

    #[test]
    fn each_bad_field_is_named() {
        let bad = Settings {
            rendezvous: node("0.0.0.0:4433"),
            forwarder: "x".repeat(MAX_FIELD + 1),
            advertise: String::from("198.51.100.2:7710"),
            port: String::from("70000"),
            ..Settings::default()
        };
        let fields: Vec<Field> = bad
            .check()
            .unwrap_err()
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(fields, [Field::Rendezvous, Field::Forwarder, Field::Port]);
    }

    #[test]
    fn saved_settings_last_between_runs() {
        let dir = temp_dir("lasts");
        let mut store = Store::open(Some(dir.clone()));
        assert_eq!(store.saved, Settings::default());
        store.save(good()).unwrap();
        assert!(store.problem.is_none(), "{:?}", store.problem);
        let again = Store::open(Some(dir.clone()));
        assert_eq!(again.saved, store.saved);
        assert!(again.problem.is_none());
        assert!(!dir.join(format!("{FILE}.new")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_settings_are_not_saved() {
        let dir = temp_dir("refused");
        let mut store = Store::open(Some(dir.clone()));
        let bad = Settings {
            port: String::from("port"),
            ..good()
        };
        assert!(store.save(bad).is_err());
        assert_eq!(store.saved, Settings::default());
        assert!(!dir.join(FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_file_is_reported_and_kept_when_saving() {
        let dir = temp_dir("bad");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), b"{ not json").unwrap();
        let mut store = Store::open(Some(dir.clone()));
        assert!(store.problem.is_some());
        assert_eq!(store.saved, Settings::default());
        assert_eq!(
            fs::read(dir.join(FILE)).unwrap(),
            b"{ not json",
            "left in place"
        );
        let settings = good();
        store.save(settings.clone()).unwrap();
        assert_eq!(
            fs::read(dir.join(BACKUP)).unwrap(),
            b"{ not json",
            "kept aside"
        );
        assert_eq!(Store::open(Some(dir.clone())).saved, settings);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_file_is_not_read_whole() {
        let dir = temp_dir("large");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), vec![b' '; MAX_FILE as usize + 10]).unwrap();
        assert!(Store::open(Some(dir.clone())).problem.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_edited_bad_value_falls_back_to_the_defaults() {
        let dir = temp_dir("hand");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), br#"{"port":"nonsense","extra":1}"#).unwrap();
        let store = Store::open(Some(dir.clone()));
        assert_eq!(
            store.saved.port, "nonsense",
            "unknown keys are ignored, the rest loads"
        );
        assert!(store.problem.is_some(), "the bad port is reported");
        assert_eq!(store.config().port, 0, "and not used");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_name_only_set_variables() {
        let env = |name: &str| match name {
            plan::ENV_PORT => Some(String::from("9000")),
            plan::ENV_FORWARDER => Some(String::from("  ")),
            _ => None,
        };
        assert_eq!(Settings::overridden(env), [Field::Port]);
    }
}
