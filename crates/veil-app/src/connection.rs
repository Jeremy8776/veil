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
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::data_use::DataUse;
use crate::engine::plan::{self, NetConfig};

const FILE: &str = "connection.json";
const BACKUP: &str = "connection.json.bak";
/// Bounded storage for the active fields and up to sixteen named setups.
const MAX_FILE: u64 = 256 * 1024;
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
    pub local_only: bool,
    pub nearby_discovery: bool,
    pub rendezvous: String,
    pub forwarder: String,
    pub advertise: String,
    pub port: String,
    /// Ask the router to open the call port while hosting. Off until the person says yes.
    pub open_port: bool,
    /// The question before the first hosted call has been answered, either way.
    pub asked_open_port: bool,
    /// When joining, reach others only through the relay, so they never learn your address.
    pub hide_address: bool,
    /// How much data calls use (`data_use.rs`). Changed from the pre-join screen and the
    /// call, not from Settings > Connection.
    pub data_use: DataUse,
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
            local_only: self.local_only,
            nearby_discovery: self.nearby_discovery,
            open_port: self.open_port,
            relay_only: self.hide_address,
            ..NetConfig::default()
        };
        for field in Field::ALL {
            // Keep internet settings on disk for later, but do not let an unavailable or
            // mistyped server prevent a local call. The listening port still matters.
            if self.local_only && field != Field::Port {
                continue;
            }
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
            Ok(config.enforce_policy())
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

/// Why a draft cannot be applied or saved.
#[derive(Debug)]
pub enum SaveError {
    Fields(Vec<(Field, String)>),
    Conflict(&'static str),
    Setup(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fields(_) => {
                formatter.write_str("This setup has invalid fields. Open Connection to fix them.")
            }
            Self::Conflict(problem) => formatter.write_str(problem),
            Self::Setup(problem) => formatter.write_str(problem),
        }
    }
}

pub const MAX_SETUP_NAME: usize = 40;
pub const MAX_SETUPS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Setup {
    pub id: u64,
    pub name: String,
    pub settings: Settings,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedFile {
    #[serde(flatten)]
    settings: Settings,
    setups: Vec<Setup>,
    active_setup: Option<u64>,
}

impl SavedFile {
    fn validate(&self) -> Result<(), String> {
        for settings in
            std::iter::once(&self.settings).chain(self.setups.iter().map(|setup| &setup.settings))
        {
            if Field::ALL
                .iter()
                .any(|field| settings.field(*field).chars().count() > MAX_FIELD)
            {
                return Err(String::from("a saved connection field is too long"));
            }
        }
        if self.setups.is_empty() && self.active_setup.is_none() {
            return Ok(());
        }
        if self.setups.len() > MAX_SETUPS {
            return Err(String::from("too many saved setups"));
        }
        for (index, setup) in self.setups.iter().enumerate() {
            validate_name(&setup.name)?;
            if setup.id == 0
                || self.setups[..index]
                    .iter()
                    .any(|s| s.id == setup.id || s.name.eq_ignore_ascii_case(&setup.name))
            {
                return Err(String::from("duplicate setup name or identifier"));
            }
        }
        let active = self
            .setups
            .iter()
            .find(|s| Some(s.id) == self.active_setup)
            .ok_or_else(|| String::from("the active setup is missing"))?;
        if active.settings != self.settings {
            return Err(String::from(
                "the active setup does not match its saved settings",
            ));
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name.chars().count() > MAX_SETUP_NAME
        || name.chars().any(char::is_control)
    {
        Err(format!(
            "Use a setup name from 1 to {MAX_SETUP_NAME} characters."
        ))
    } else {
        Ok(())
    }
}

/// A recent, explicit server check. It never claims that incoming calls can reach this host.
#[derive(Clone)]
pub struct Checked {
    pub config: NetConfig,
    pub at: std::time::Instant,
    pub servers_reachable: Option<bool>,
}

/// The saved settings and where they live.
pub struct Store {
    dir: Option<PathBuf>,
    pub saved: Settings,
    setups: Vec<Setup>,
    active_setup: u64,
    pub checked: Option<Checked>,
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
            setups: vec![Setup {
                id: 1,
                name: String::from("Default"),
                settings: Settings::default(),
            }],
            active_setup: 1,
            checked: None,
            bad_file: false,
            problem: None,
        };
        let Some(dir) = store.dir.clone() else {
            return store;
        };
        match load(&dir) {
            Ok(Some(file)) => {
                let settings = file.settings;
                if !file.setups.is_empty() {
                    store.setups = file.setups;
                    store.active_setup = file.active_setup.unwrap_or(1);
                } else {
                    store.setups[0].settings = settings.clone();
                }
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
            NetConfig {
                local_only: self.saved.local_only,
                nearby_discovery: self.saved.nearby_discovery,
                // A broken file must not quietly drop a privacy choice either.
                relay_only: self.saved.hide_address,
                ..NetConfig::default()
            }
            .enforce_policy()
        });
        saved.with_env(|name| std::env::var(name).ok())
    }

    /// The same effective settings without logging invalid overrides on every UI frame.
    pub fn summary_config(&self) -> Result<NetConfig, &'static str> {
        let config = self
            .saved
            .check()
            .map_err(|_| "Fix the saved connection fields")?;
        let config = config.with_env(|name| {
            let value = std::env::var(name).ok()?;
            let valid = match name {
                plan::ENV_RENDEZVOUS | plan::ENV_FORWARDER => plan::parse_node(&value).is_ok(),
                plan::ENV_ADVERTISE => plan::parse_advertise(&value).is_ok(),
                plan::ENV_PORT => plan::parse_port(&value).is_ok(),
                _ => false,
            };
            valid.then_some(value)
        });
        if config.conflict().is_some() {
            Err("Resolve Local only / Hide my address")
        } else {
            Ok(config)
        }
    }

    /// Saves a new data use choice and keeps every other setting as saved. If the saved
    /// settings do not pass their checks (edited by hand), the choice lasts this run only.
    pub fn set_data_use(&mut self, data_use: DataUse) {
        let mut settings = self.saved.clone();
        settings.data_use = data_use;
        if self.save(settings.clone()).is_err() {
            self.saved = settings;
        }
    }

    /// Checks and saves the active setup, including choices made before a call.
    pub fn save(&mut self, settings: Settings) -> Result<(), SaveError> {
        let name = self.active_name().to_owned();
        self.save_setup(Some(self.active_setup), &name, settings)
    }

    pub fn setups(&self) -> &[Setup] {
        &self.setups
    }
    pub fn active_id(&self) -> u64 {
        self.active_setup
    }
    pub fn active_name(&self) -> &str {
        self.setups
            .iter()
            .find(|setup| setup.id == self.active_setup)
            .map_or("Default", |setup| setup.name.as_str())
    }

    /// A setup is made active only after all validation has succeeded.
    pub fn save_setup(
        &mut self,
        id: Option<u64>,
        name: &str,
        settings: Settings,
    ) -> Result<(), SaveError> {
        let oversized: Vec<_> = Field::ALL
            .into_iter()
            .filter(|field| settings.field(*field).chars().count() > MAX_FIELD)
            .map(|field| (field, format!("At most {MAX_FIELD} characters.")))
            .collect();
        if !oversized.is_empty() {
            return Err(SaveError::Fields(oversized));
        }
        let config = settings.check().map_err(SaveError::Fields)?;
        if let Some(problem) = config.conflict() {
            return Err(SaveError::Conflict(problem));
        }
        let name = name.trim();
        validate_name(name).map_err(SaveError::Setup)?;
        if self
            .setups
            .iter()
            .any(|s| Some(s.id) != id && s.name.eq_ignore_ascii_case(name))
        {
            return Err(SaveError::Setup(String::from(
                "Use a different setup name.",
            )));
        }
        let id = if let Some(id) = id {
            if !self.setups.iter().any(|s| s.id == id) {
                return Err(SaveError::Setup(String::from(
                    "This setup no longer exists.",
                )));
            }
            id
        } else {
            if self.setups.len() >= MAX_SETUPS {
                return Err(SaveError::Setup(format!(
                    "Keep up to {MAX_SETUPS} saved setups. Remove one first."
                )));
            }
            self.setups
                .iter()
                .map(|s| s.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| SaveError::Setup(String::from("Cannot create another setup.")))?
        };
        let setup = Setup {
            id,
            name: name.to_owned(),
            settings: settings.clone(),
        };
        if let Some(existing) = self.setups.iter_mut().find(|s| s.id == id) {
            *existing = setup;
        } else {
            self.setups.push(setup);
        }
        self.active_setup = id;
        self.saved = settings;
        self.persist();
        Ok(())
    }

    pub fn activate(&mut self, id: u64) -> Result<(), SaveError> {
        let setup = self
            .setups
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| SaveError::Setup(String::from("This setup no longer exists.")))?;
        self.save_setup(Some(id), &setup.name, setup.settings)
    }

    /// The active setup is never removed implicitly.
    pub fn remove_setup(&mut self, id: u64) -> Result<(), String> {
        if id == self.active_setup {
            return Err(String::from(
                "Switch to another setup before removing this one.",
            ));
        }
        let index = self
            .setups
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| String::from("This setup no longer exists."))?;
        self.setups.remove(index);
        self.persist();
        Ok(())
    }

    fn persist(&mut self) {
        // Runtime-only choices can still change `saved` after a validation failure.
        // Keep the active snapshot consistent when another setup is later removed.
        if let Some(active) = self
            .setups
            .iter_mut()
            .find(|setup| setup.id == self.active_setup)
        {
            active.settings = self.saved.clone();
        }
        let Some(dir) = self.dir.clone() else {
            self.problem = Some(String::from(
                "Veil has no folder to save in here, so these settings last this run only.",
            ));
            return;
        };
        if self.bad_file {
            // Kept, not deleted: it may hold values someone typed by hand.
            if let Err(e) = fs::rename(dir.join(FILE), dir.join(BACKUP))
                && e.kind() != std::io::ErrorKind::NotFound
            {
                self.problem = Some(format!(
                    "The old settings file could not be moved aside ({e}), so it was left as \
                     it is. These settings last this run only."
                ));
                return;
            }
            self.bad_file = false;
        }
        match crate::json_file::write(
            &dir,
            FILE,
            &SavedFile {
                settings: self.saved.clone(),
                setups: self.setups.clone(),
                active_setup: Some(self.active_setup),
            },
        ) {
            Ok(()) => self.problem = None,
            Err(e) => {
                self.problem = Some(format!(
                    "Your settings could not be saved ({e}). They last this run only."
                ));
            }
        }
    }
}

/// `Ok(None)` when there is no file yet.
fn load(dir: &Path) -> Result<Option<SavedFile>, String> {
    let file: Option<SavedFile> = crate::json_file::load(dir, FILE, MAX_FILE)?;
    if let Some(file) = &file {
        file.validate()?;
    }
    Ok(file)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use veil_net::NodeAddr;
    use veil_security::Identity;

    use super::*;

    #[test]
    fn legacy_settings_migrate_and_named_setups_round_trip_independently() {
        let dir = temp_dir("setups-migration");
        crate::json_file::write(&dir, FILE, &good()).unwrap();
        let mut store = Store::open(Some(dir.clone()));
        let original = store.saved.clone();
        assert_eq!(store.active_name(), "Default");
        assert_eq!(store.setups()[0].settings, original);
        let first = store.active_id();
        let mut work = good();
        work.port = "7711".into();
        store.save_setup(None, "Work VPS", work.clone()).unwrap();
        let work_id = store.active_id();
        let mut store = Store::open(Some(dir.clone()));
        assert_eq!(store.setups().len(), 2);
        assert_eq!(store.saved, work);
        assert_eq!(store.active_name(), "Work VPS");
        store.activate(first).unwrap();
        assert_eq!(store.saved, original);
        store.set_data_use(DataUse::AudioOnly);
        assert_eq!(
            store
                .setups()
                .iter()
                .find(|s| s.id == work_id)
                .unwrap()
                .settings,
            work
        );
        let store = Store::open(Some(dir.clone()));
        assert_eq!(store.saved.data_use, DataUse::AudioOnly);
        assert_eq!(store.active_id(), first);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_validation_and_removal_preserve_the_active_setup() {
        let dir = temp_dir("setups-validation");
        let mut store = Store::open(Some(dir.clone()));
        store.save(good()).unwrap();
        let first = store.active_id();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        for name in ["", "default", "bad\nname"] {
            assert!(matches!(
                store.save_setup(None, name, good()),
                Err(SaveError::Setup(_))
            ));
            assert_eq!(fs::read(dir.join(FILE)).unwrap(), bytes);
        }
        assert!(store.remove_setup(first).is_err());
        store.save_setup(None, "Gaming VPS", good()).unwrap();
        let active = store.active_id();
        store.remove_setup(first).unwrap();
        let reloaded = Store::open(Some(dir.clone()));
        assert_eq!(reloaded.setups().len(), 1);
        assert_eq!(reloaded.active_id(), active);
        assert_eq!(reloaded.active_name(), "Gaming VPS");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_setup_metadata_is_preserved_as_a_bad_file() {
        let dir = temp_dir("setups-corrupt");
        fs::create_dir_all(&dir).unwrap();
        let bytes = br#"{"setups":[],"active_setup":42}"#;
        fs::write(dir.join(FILE), bytes).unwrap();
        let mut store = Store::open(Some(dir.clone()));
        assert!(store.problem.is_some());
        assert_eq!(fs::read(dir.join(FILE)).unwrap(), bytes);
        store.save(Settings::default()).unwrap();
        assert_eq!(fs::read(dir.join(BACKUP)).unwrap(), bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_choice_and_setup_removal_keep_the_active_snapshot_consistent() {
        let dir = temp_dir("setups-runtime-choice");
        let mut store = Store::open(Some(dir.clone()));
        let first = store.active_id();
        store
            .save_setup(None, "Local", Settings::default())
            .unwrap();
        store.saved.local_only = true;
        store.saved.hide_address = true;
        store.set_data_use(DataUse::AudioOnly);
        store.remove_setup(first).unwrap();
        let again = Store::open(Some(dir.clone()));
        assert!(again.problem.is_none());
        assert!(again.saved.local_only && again.saved.hide_address);
        assert_eq!(again.saved.data_use, DataUse::AudioOnly);
        assert!(again.config().conflict().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inactive_setup_fields_are_bounded_on_read() {
        let dir = temp_dir("setups-field-limit");
        let settings = Settings::default();
        let file = SavedFile {
            settings: settings.clone(),
            setups: vec![
                Setup {
                    id: 1,
                    name: "Default".into(),
                    settings,
                },
                Setup {
                    id: 2,
                    name: "Oversized".into(),
                    settings: Settings {
                        rendezvous: "x".repeat(MAX_FIELD + 1),
                        ..Settings::default()
                    },
                },
            ],
            active_setup: Some(1),
        };
        crate::json_file::write(&dir, FILE, &file).unwrap();
        assert!(Store::open(Some(dir.clone())).problem.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_only_ignores_bad_internet_fields_but_validates_listening_port() {
        let mut settings = Settings {
            local_only: true,
            rendezvous: "invalid saved host".into(),
            forwarder: "also invalid".into(),
            advertise: "not an address".into(),
            open_port: true,
            hide_address: true,
            ..Settings::default()
        };
        let config = settings.check().unwrap();
        assert!(config.local_only && !config.open_port && config.relay_only);
        assert!(
            config.conflict().is_some(),
            "Hide my address is kept, so the call stops"
        );
        assert!(
            config.rendezvous.is_none()
                && config.forwarder.is_none()
                && config.advertise.is_empty()
        );
        settings.port = "invalid".into();
        assert!(settings.check().is_err());
    }

    #[test]
    fn malformed_port_does_not_silently_disable_local_only() {
        let mut store = Store::open(None);
        store.saved.local_only = true;
        store.saved.port = "invalid".into();
        assert!(store.config().local_only);
        store.saved.hide_address = true;
        assert!(store.config().relay_only, "nor Hide my address");
    }

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
            hide_address: true,
            data_use: DataUse::Saver,
            local_only: false,
            nearby_discovery: false,
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
    fn data_use_and_hiding_are_kept_and_default_to_auto_and_off() {
        assert_eq!(Settings::default().data_use, DataUse::Auto);
        assert!(!Settings::default().check().unwrap().relay_only);
        assert!(good().check().unwrap().relay_only);
        let dir = temp_dir("data-use");
        fs::create_dir_all(&dir).unwrap();
        // A file saved before these choices existed.
        fs::write(dir.join(FILE), br#"{"port":"7710","open_port":true}"#).unwrap();
        let mut store = Store::open(Some(dir.clone()));
        assert_eq!(store.saved.data_use, DataUse::Auto);
        assert!(!store.saved.hide_address);
        store.set_data_use(DataUse::AudioOnly);
        let again = Store::open(Some(dir.clone()));
        assert_eq!(again.saved.data_use, DataUse::AudioOnly);
        assert!(again.saved.open_port, "other settings are kept");
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
    fn conflicting_privacy_choices_cannot_replace_saved_settings() {
        let dir = temp_dir("privacy-conflict");
        let mut store = Store::open(Some(dir.clone()));
        store.save(good()).unwrap();
        let baseline = store.saved.clone();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        let mut draft = baseline.clone();
        draft.local_only = true;
        assert!(matches!(
            store.save(draft.clone()),
            Err(SaveError::Conflict(_))
        ));
        assert!(
            draft.local_only && draft.hide_address,
            "neither choice is silently dropped"
        );
        assert_eq!(store.saved, baseline);
        assert_eq!(fs::read(dir.join(FILE)).unwrap(), bytes);

        draft.hide_address = false;
        store.save(draft.clone()).unwrap();
        assert_eq!(Store::open(Some(dir.clone())).saved, draft);
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
