//! Your device and video choices, kept between runs: camera, microphone, speaker, filter or
//! imported LUT (by name), background blur, both effect strengths, mirror, both volumes and
//! the sound switches (Noise suppression, Auto volume, Remove noise from others), and the
//! host's Lobby alerts switch.
//!
//! Purpose: [`Store::observe`] is fed what [`LocalMedia`] holds once a frame, so a choice
//! made anywhere (the pre-join card or a call menu) is saved without each control having to
//! remember to. A change is written once it has stayed the same for [`SETTLE`], so dragging a
//! volume slider writes one file, not one per frame. Saved to `preferences.json` in Veil's
//! folder.
//!
//! Invariants:
//! - The file holds device ids and plain choices, nothing secret, so it is plain JSON.
//! - A saved device that is not plugged in now is kept, and the default is used for this run.
//! - A file that does not load is logged and replaced on the next change: losing these
//!   choices costs a few clicks, unlike `connection.json`, which keeps a backup.
//! - Anything still waiting to be written is written when the store is dropped.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use veil_media::{Blur, Filter};

use crate::devices::{Device, DeviceLists};
use crate::media::LocalMedia;

const FILE: &str = "preferences.json";
/// Far more than these few fields need.
const MAX_FILE: u64 = 16 * 1024;
/// How long a change must stay put before it is written.
pub const SETTLE: Duration = Duration::from_millis(600);

/// What is remembered. Every field has a default, so an older or partial file still loads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub display_name: String,
    pub camera: Option<String>,
    pub mic: Option<String>,
    pub speaker: Option<String>,
    #[serde(with = "filter_key")]
    pub filter: Filter,
    /// An imported LUT's name (`luts.rs`), used instead of `filter` while it still exists.
    pub lut: Option<String>,
    /// Filter or LUT strength in percent; `None` until first changed (100%).
    pub look_strength: Option<u8>,
    #[serde(with = "blur_key")]
    pub blur: Blur,
    /// Background blur strength in percent; `None` until first changed (100%).
    pub blur_strength: Option<u8>,
    pub mirror: bool,
    /// `None` until first changed: Veil's own default is used.
    pub mic_volume: Option<f32>,
    pub speaker_volume: Option<f32>,
    /// The sound switches; `None` until first changed, like the volumes.
    pub noise_suppression: Option<bool>,
    pub auto_gain: Option<bool>,
    pub clean_others: Option<bool>,
    /// Notify the host when someone starts waiting (`lobby_alert.rs`); `None` until first
    /// changed (on).
    pub lobby_alerts: Option<bool>,
    /// Keep faces in view in cropped tiles (`framing.rs`); `None` until first changed (on).
    pub face_framing: Option<bool>,
}

impl Prefs {
    /// Sets the filter or LUT, blur, strengths, mirror, volumes and sound switches. Devices
    /// wait for the lists ([`Prefs::devices`]), since a saved one may not be plugged in.
    pub fn apply(&self, media: &LocalMedia, luts: &crate::luts::Library) {
        media.set_filter(self.filter);
        if let Some(look) = self.lut.as_deref().and_then(|name| luts.load(name)) {
            media.set_lut(Some(look));
        }
        if let Some(percent) = self.look_strength {
            media.set_look_strength(percent);
        }
        media.set_blur(self.blur);
        if let Some(percent) = self.blur_strength {
            media.set_blur_strength(percent);
        }
        media.set_mirror(self.mirror);
        if let Some(on) = self.noise_suppression {
            media.set_noise_suppression(on);
        }
        if let Some(on) = self.auto_gain {
            media.set_auto_gain(on);
        }
        if let Some(on) = self.clean_others {
            media.playback().clean_others.store(on, Ordering::Relaxed);
        }
        if let Some(v) = self.mic_volume {
            media.set_mic_volume(v);
        }
        if let Some(v) = self.speaker_volume {
            media.set_speaker_volume(v);
        }
    }

    /// The saved devices that are in `lists` now; the rest are the default.
    pub fn devices(&self, lists: &DeviceLists) -> crate::devices::Choice {
        crate::devices::Choice {
            camera: present(&self.camera, &lists.cameras),
            mic: present(&self.mic, &lists.mics),
            speaker: present(&self.speaker, &lists.speakers),
        }
    }

    /// What `media` holds now, keeping a saved device that is unplugged rather than
    /// forgetting it.
    pub fn current(media: &LocalMedia, saved: &Prefs, lists: &DeviceLists) -> Self {
        let choice = media.choice();
        Self {
            display_name: saved.display_name.clone(),
            camera: kept(&saved.camera, &choice.camera, &lists.cameras),
            mic: kept(&saved.mic, &choice.mic, &lists.mics),
            speaker: kept(&saved.speaker, &choice.speaker, &lists.speakers),
            filter: media.filter(),
            lut: media.lut().map(|look| look.name),
            look_strength: Some(media.look_strength()),
            blur: media.blur(),
            blur_strength: Some(media.blur_strength()),
            mirror: media.mirror(),
            mic_volume: Some(media.mic_volume()),
            speaker_volume: Some(media.speaker_volume()),
            noise_suppression: Some(media.noise_suppression()),
            auto_gain: Some(media.auto_gain()),
            clean_others: Some(media.playback().clean_others.load(Ordering::Relaxed)),
            lobby_alerts: saved.lobby_alerts,
            face_framing: saved.face_framing,
        }
    }
}

fn present(id: &Option<String>, list: &[Device]) -> Option<String> {
    id.clone().filter(|id| list.iter().any(|d| &d.id == id))
}

/// The device to remember: the one in use, unless that is the default only because the saved
/// one is not plugged in.
/// shortcut: choosing "Default" while the saved device is unplugged is not remembered; track
/// an explicit pick if anyone asks for that.
fn kept(saved: &Option<String>, current: &Option<String>, list: &[Device]) -> Option<String> {
    match (saved, current) {
        (Some(id), None) if !list.iter().any(|d| &d.id == id) => Some(id.clone()),
        _ => current.clone(),
    }
}

/// The saved preferences, what waits to be written, and where they live.
pub struct Store {
    dir: Option<PathBuf>,
    pub saved: Prefs,
    pending: Option<(Prefs, Instant)>,
}

impl Store {
    /// Opens the preferences in `dir`. Missing or unreadable is the defaults.
    pub fn open(dir: Option<PathBuf>) -> Self {
        let saved = match dir
            .as_deref()
            .map(|d| crate::json_file::load(d, FILE, MAX_FILE))
        {
            Some(Ok(Some(prefs))) => prefs,
            Some(Err(why)) => {
                crate::note!("preferences not loaded: {why}");
                Prefs::default()
            }
            _ => Prefs::default(),
        };
        Self {
            dir,
            saved,
            pending: None,
        }
    }

    /// Notes what is chosen `now`; writes it once it has stayed the same for [`SETTLE`].
    /// Returns true while a change still waits, so the caller can ask for a frame later.
    pub fn observe(&mut self, now: Prefs, at: Instant) -> bool {
        if now == self.saved {
            self.pending = None;
            return false;
        }
        match &self.pending {
            Some((waiting, since)) if *waiting == now => {
                if at.duration_since(*since) >= SETTLE {
                    self.saved = now;
                    self.pending = None;
                    self.write();
                    return false;
                }
            }
            _ => self.pending = Some((now, at)),
        }
        true
    }

    fn write(&self) {
        let Some(dir) = &self.dir else { return };
        if let Err(why) = crate::json_file::write(dir, FILE, &self.saved) {
            crate::note!("preferences not saved: {why}");
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some((prefs, _)) = self.pending.take() {
            self.saved = prefs;
            self.write();
        }
    }
}

/// Filter and blur are saved by name, so reordering the enums never changes a saved choice.
fn filter_name(filter: Filter) -> &'static str {
    match filter {
        Filter::None => "none",
        Filter::Warm => "warm",
        Filter::Cool => "cool",
        Filter::BlackWhite => "black-white",
        Filter::Bright => "bright",
    }
}

fn blur_name(blur: Blur) -> &'static str {
    match blur {
        Blur::Off => "off",
        Blur::Light => "light",
        Blur::Strong => "strong",
    }
}

mod filter_key {
    use serde::{Deserialize, Deserializer, Serializer};
    use veil_media::Filter;

    pub fn serialize<S: Serializer>(filter: &Filter, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(super::filter_name(*filter))
    }

    /// An unknown name is the default, not an error, so one bad field keeps the rest.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Filter, D::Error> {
        let name = String::deserialize(d)?;
        Ok(Filter::ALL
            .into_iter()
            .find(|f| super::filter_name(*f) == name)
            .unwrap_or_default())
    }
}

mod blur_key {
    use serde::{Deserialize, Deserializer, Serializer};
    use veil_media::Blur;

    pub fn serialize<S: Serializer>(blur: &Blur, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(super::blur_name(*blur))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Blur, D::Error> {
        let name = String::deserialize(d)?;
        Ok(Blur::ALL
            .into_iter()
            .find(|b| super::blur_name(*b) == name)
            .unwrap_or_default())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-prefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn device(id: &str) -> Device {
        Device {
            name: id.to_uppercase(),
            id: id.to_string(),
        }
    }

    fn chosen() -> Prefs {
        Prefs {
            display_name: String::from("Alex"),
            camera: Some(String::from("cam-2")),
            mic: Some(String::from("mic-1")),
            speaker: None,
            filter: Filter::Warm,
            lut: Some(String::from("Film")),
            look_strength: Some(60),
            blur: Blur::Strong,
            blur_strength: Some(35),
            mirror: true,
            mic_volume: Some(1.5),
            speaker_volume: Some(0.4),
            noise_suppression: Some(false),
            auto_gain: Some(true),
            clean_others: Some(true),
            lobby_alerts: Some(false),
            face_framing: Some(false),
        }
    }

    #[test]
    fn a_change_is_written_once_it_settles() {
        let dir = temp_dir("settle");
        let mut store = Store::open(Some(dir.clone()));
        let t = Instant::now();
        assert!(store.observe(chosen(), t), "waits");
        assert!(store.observe(chosen(), t + SETTLE / 2), "still waits");
        assert!(!dir.join(FILE).exists());
        assert!(!store.observe(chosen(), t + SETTLE), "written");
        assert_eq!(Store::open(Some(dir.clone())).saved, chosen());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_change_that_keeps_moving_restarts_the_wait() {
        let mut store = Store::open(None);
        let t = Instant::now();
        let mut first = chosen();
        first.mic_volume = Some(1.0);
        store.observe(first, t);
        assert!(
            store.observe(chosen(), t + SETTLE),
            "a new value starts again"
        );
        assert!(!store.observe(chosen(), t + SETTLE * 2));
        assert_eq!(store.saved, chosen());
    }

    #[test]
    fn a_waiting_change_is_written_on_drop() {
        let dir = temp_dir("drop");
        let mut store = Store::open(Some(dir.clone()));
        store.observe(chosen(), Instant::now());
        drop(store);
        assert_eq!(Store::open(Some(dir.clone())).saved, chosen());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_or_odd_file_still_loads() {
        let dir = temp_dir("odd");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE),
            br#"{"blur":"strong","filter":"sepia","extra":1}"#,
        )
        .unwrap();
        let saved = Store::open(Some(dir.clone())).saved.clone();
        assert_eq!(saved.blur, Blur::Strong);
        assert_eq!(saved.filter, Filter::None, "an unknown name is the default");
        assert_eq!(saved.camera, None);
        fs::write(dir.join(FILE), b"{ not json").unwrap();
        assert_eq!(Store::open(Some(dir.clone())).saved, Prefs::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_filter_and_blur_survives_a_round_trip() {
        for filter in Filter::ALL {
            for blur in Blur::ALL {
                let prefs = Prefs {
                    filter,
                    blur,
                    ..Prefs::default()
                };
                let text = serde_json::to_string(&prefs).unwrap();
                assert_eq!(serde_json::from_str::<Prefs>(&text).unwrap(), prefs);
            }
        }
    }

    #[test]
    fn an_unplugged_device_is_remembered_but_not_used() {
        let lists = DeviceLists {
            cameras: vec![device("cam-1")],
            mics: vec![device("mic-1")],
            speakers: vec![],
        };
        let use_now = chosen().devices(&lists);
        assert_eq!(use_now.camera, None, "cam-2 is not plugged in");
        assert_eq!(use_now.mic.as_deref(), Some("mic-1"));
        let saved = Some(String::from("cam-2"));
        assert_eq!(kept(&saved, &None, &lists.cameras), saved, "kept");
        let picked = Some(String::from("cam-1"));
        assert_eq!(
            kept(&saved, &picked, &lists.cameras),
            picked,
            "a new pick wins"
        );
        let both = vec![device("cam-1"), device("cam-2")];
        assert_eq!(
            kept(&saved, &None, &both),
            None,
            "default chosen while it is plugged in"
        );
    }
}
