//! The cameras, microphones and speakers on this machine, and which ones you chose.
//!
//! Purpose: one list per kind for the pre-join pickers, read from `veil-media`. A choice of
//! `None` means the default: the first camera, and Windows' default communications microphone
//! and speaker.
//!
//! Invariants:
//! - Listing never fails the screen: a device kind that cannot be listed comes back empty, and
//!   the picker then offers only the default.
//! - Names and ids stay on this device; nothing about them is sent.

/// One device as a picker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceLists {
    pub cameras: Vec<Device>,
    pub mics: Vec<Device>,
    pub speakers: Vec<Device>,
}

/// Your choice per kind; `None` is the default device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub camera: Option<String>,
    pub mic: Option<String>,
    pub speaker: Option<String>,
}

impl DeviceLists {
    /// Reads every list now. Takes a moment; call it when a screen opens, not every frame.
    #[cfg(windows)]
    pub fn load() -> Self {
        let cameras = veil_media::cameras()
            .unwrap_or_default()
            .into_iter()
            .map(|c| Device {
                name: c.name,
                id: c.id,
            })
            .collect();
        let audio = |list: Result<Vec<veil_media::AudioDevice>, _>| {
            list.unwrap_or_default()
                .into_iter()
                .map(|d| Device {
                    name: d.name,
                    id: d.id,
                })
                .collect()
        };
        Self {
            cameras,
            mics: audio(veil_media::microphones()),
            speakers: audio(veil_media::speakers()),
        }
    }

    #[cfg(not(windows))]
    pub fn load() -> Self {
        Self::default()
    }
}

/// The name to show for `chosen` in `list`, with `default` for `None` or a device that has
/// gone (unplugged since it was chosen).
pub fn label<'a>(list: &'a [Device], chosen: Option<&str>, default: &'a str) -> &'a str {
    chosen
        .and_then(|id| list.iter().find(|d| d.id == id))
        .map_or(default, |d| d.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_fall_back_to_the_default() {
        let list = vec![Device {
            name: "Headset".into(),
            id: "a".into(),
        }];
        assert_eq!(label(&list, Some("a"), "Default"), "Headset");
        assert_eq!(label(&list, None, "Default"), "Default");
        assert_eq!(label(&list, Some("gone"), "Default"), "Default");
    }
}
