//! What the engine knows about the call, shared between its threads and the interface.
//!
//! Purpose: the session task writes [`View`] (status, invite, people, lobby, key epoch) and
//! queues chat that arrived, the decoder thread writes pictures and the playback thread
//! writes what it heard. [`apply`] turns all of that into the screens' [`Call`], once per
//! interface frame, and [`Shared::take_chat`] hands over new messages once.
//!
//! Invariants:
//! - Each lock is held only to copy or replace a value, never across a wait.
//! - Whether someone is muted, has video or is speaking is read from what actually arrived:
//!   a microphone that is off sends nothing, so silence for a second shows as muted.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui;
use veil_security::PublicId;

use super::names::labels;
use crate::convert::Rgba;
use crate::media::lock;
use crate::model::{Call, CallStatus, LinkPath, Participant, Reachable, RouterPort, Waiting};

/// Chat messages that may wait for the interface; more drop the oldest.
const CHAT_QUEUE: usize = 256;
/// No sound for this long shows someone as muted.
const QUIET: Duration = Duration::from_secs(1);
/// No picture for this long shows someone's camera as off.
const DARK: Duration = Duration::from_secs(2);
/// A decoded level above this, on the meter's 0 to 1 scale, shows someone speaking.
const SPEAKING: f32 = 0.45;

#[derive(Clone, Debug)]
pub struct Person {
    pub id: u32,
    pub device: PublicId,
    pub name: String,
    pub is_host: bool,
    pub safety: String,
    pub path: LinkPath,
}

#[derive(Clone, Debug)]
pub struct Knocker {
    pub id: u32,
    pub device: PublicId,
    pub name: String,
    pub safety: String,
    pub since: Instant,
}

#[derive(Clone, Debug)]
pub struct View {
    pub status: CallStatus,
    pub invite: String,
    /// See `Call::invite_expires` and `Call::invite_until_end`.
    pub invite_expires: u64,
    pub invite_until_end: bool,
    pub epoch: u64,
    /// Everyone but you, in roster order.
    pub people: Vec<Person>,
    pub lobby: Vec<Knocker>,
    pub router: RouterPort,
    pub reach: Reachable,
}

/// The newest picture from one person. `rgba` is taken by the interface once uploaded.
struct Picture {
    rgba: Option<Rgba>,
    at: Instant,
}

/// What the playback thread heard from one person.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    /// When the last packet arrived.
    pub at: Instant,
    /// Level of the last frame played, 0 to 1.
    pub level: f32,
}

/// When each person last showed video and sound.
#[derive(Default)]
pub struct Seen {
    pub video: HashMap<u32, Instant>,
    pub audio: HashMap<u32, Heard>,
}

pub struct Shared {
    pub view: Mutex<View>,
    pictures: Mutex<HashMap<u32, Picture>>,
    heard: Mutex<HashMap<u32, Heard>>,
    /// Chat that arrived, as (sender's device, text), not yet in the interface's call.
    chat: Mutex<Vec<(PublicId, String)>>,
    /// Each other person's tile on screen, as (participant id, width in physical pixels).
    tiles: Mutex<Vec<(u32, f32)>>,
    ctx: egui::Context,
}

impl Shared {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            view: Mutex::new(View {
                status: CallStatus::Connecting,
                invite: String::new(),
                invite_expires: 0,
                invite_until_end: false,
                epoch: 0,
                people: Vec::new(),
                lobby: Vec::new(),
                router: RouterPort::Off,
                reach: Reachable::Unknown,
            }),
            pictures: Mutex::default(),
            heard: Mutex::default(),
            chat: Mutex::default(),
            tiles: Mutex::default(),
            ctx,
        }
    }

    /// The interface's tiles this frame. Does not repaint: the session reads it on its tick.
    pub fn set_tiles(&self, tiles: Vec<(u32, f32)>) {
        *lock(&self.tiles) = tiles;
    }

    pub fn tiles(&self) -> Vec<(u32, f32)> {
        lock(&self.tiles).clone()
    }

    /// Changes the view and asks the interface to draw it.
    /// A change of status (connecting, lobby, live, ended and why) goes to the log.
    pub fn update(&self, change: impl FnOnce(&mut View)) {
        let mut view = lock(&self.view);
        let before = std::mem::discriminant(&view.status);
        change(&mut view);
        if std::mem::discriminant(&view.status) != before {
            let status = view.status.clone();
            drop(view);
            crate::note!("call: {status:?}");
        } else {
            drop(view);
        }
        self.ctx.request_repaint();
    }

    pub fn put_picture(&self, id: u32, rgba: Rgba) {
        lock(&self.pictures).insert(
            id,
            Picture {
                rgba: Some(rgba),
                at: Instant::now(),
            },
        );
        self.ctx.request_repaint();
    }

    pub fn forget_picture(&self, id: u32) {
        lock(&self.pictures).remove(&id);
    }

    pub fn heard(&self, id: u32, change: impl FnOnce(&mut Heard)) {
        let mut heard = lock(&self.heard);
        let entry = heard.entry(id).or_insert(Heard {
            at: Instant::now(),
            level: 0.0,
        });
        change(entry);
    }

    pub fn forget_heard(&self, id: u32) {
        lock(&self.heard).remove(&id);
    }

    /// Queues a chat message that arrived for the interface.
    pub fn push_chat(&self, from: PublicId, text: String) {
        let mut chat = lock(&self.chat);
        if chat.len() >= CHAT_QUEUE {
            chat.remove(0);
        }
        chat.push((from, text));
        drop(chat);
        self.ctx.request_repaint();
    }

    /// Chat that arrived since the last call.
    pub fn take_chat(&self) -> Vec<(PublicId, String)> {
        std::mem::take(&mut *lock(&self.chat))
    }

    pub fn take_pictures(&self) -> Vec<(u32, Rgba)> {
        lock(&self.pictures)
            .iter_mut()
            .filter_map(|(id, p)| p.rgba.take().map(|rgba| (*id, rgba)))
            .collect()
    }

    pub fn seen(&self) -> Seen {
        Seen {
            video: lock(&self.pictures)
                .iter()
                .map(|(id, p)| (*id, p.at))
                .collect(),
            audio: lock(&self.heard).clone(),
        }
    }
}

/// Copies the engine's view into the screens' call. Your own entry is left to the interface
/// (devices, hand); everyone else is rebuilt from the view each time. `verified` says
/// whether you marked a device as verified. Returns the name shown for each device, which
/// chat uses too, so a name two people share carries a key prefix everywhere.
pub fn apply(
    view: &View,
    seen: &Seen,
    verified: impl Fn(&PublicId) -> bool,
    call: &mut Call,
    now: Instant,
) -> HashMap<PublicId, String> {
    call.status = view.status.clone();
    call.invite.clone_from(&view.invite);
    call.invite_expires = view.invite_expires;
    call.invite_until_end = view.invite_until_end;
    call.router.clone_from(&view.router);
    call.reach.clone_from(&view.reach);
    call.epoch = view.epoch;
    call.participants.retain(|p| p.is_you);
    let yours = call.you().map(|p| p.name.clone());
    let shown = labels(
        yours.as_deref(),
        view.people
            .iter()
            .map(|p| (&p.device, p.name.as_str()))
            .chain(view.lobby.iter().map(|k| (&k.device, k.name.as_str()))),
    );
    let label = |device: &PublicId| shown.get(device).cloned().unwrap_or_default();
    for person in &view.people {
        let heard = seen
            .audio
            .get(&person.id)
            .filter(|h| now.saturating_duration_since(h.at) < QUIET);
        let camera = seen
            .video
            .get(&person.id)
            .is_some_and(|at| now.saturating_duration_since(*at) < DARK);
        call.participants.push(Participant {
            id: person.id,
            name: label(&person.device),
            is_you: false,
            is_host: person.is_host,
            muted: heard.is_none(),
            camera,
            // Not sent yet: only your own hand shows.
            hand_raised: false,
            speaking: heard.is_some_and(|h| h.level > SPEAKING),
            path: person.path,
            safety: person.safety.clone(),
            verified: verified(&person.device),
        });
    }
    call.lobby = view
        .lobby
        .iter()
        .map(|k| Waiting {
            id: k.id,
            name: label(&k.device),
            since: k.since,
            safety: k.safety.clone(),
            verified: verified(&k.device),
        })
        .collect();
    shown
}

#[cfg(test)]
mod tests {
    use super::super::short;
    use super::*;

    fn view(now: Instant) -> View {
        View {
            status: CallStatus::Live,
            invite: "veil://join/x".into(),
            invite_expires: 0,
            invite_until_end: false,
            epoch: 3,
            people: vec![
                Person {
                    id: 1,
                    device: PublicId([1; 32]),
                    name: "Host".into(),
                    is_host: true,
                    safety: "12345".into(),
                    path: LinkPath::Direct,
                },
                Person {
                    id: 2,
                    device: PublicId([2; 32]),
                    name: "Guest ab12cd34".into(),
                    is_host: false,
                    safety: "67890".into(),
                    path: LinkPath::Pending,
                },
            ],
            lobby: vec![Knocker {
                id: 3,
                device: PublicId([3; 32]),
                name: "Guest".into(),
                safety: "11111".into(),
                since: now,
            }],
            router: RouterPort::Off,
            reach: Reachable::Unknown,
        }
    }

    #[test]
    fn people_show_what_arrived() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", false, now);
        let mut seen = Seen::default();
        seen.video.insert(1, now);
        seen.audio.insert(
            1,
            Heard {
                at: now,
                level: 0.8,
            },
        );
        seen.audio.insert(
            2,
            Heard {
                at: now.checked_sub(Duration::from_secs(5)).unwrap_or(now),
                level: 0.9,
            },
        );
        apply(
            &view(now),
            &seen,
            |d| *d == PublicId([3; 32]),
            &mut call,
            now,
        );
        assert_eq!(call.status, CallStatus::Live);
        assert_eq!(call.epoch, 3);
        assert_eq!(call.participants.len(), 3);
        assert!(call.participants[0].is_you);
        let host = &call.participants[1];
        assert!(host.camera && !host.muted && host.speaking && host.is_host);
        let guest = &call.participants[2];
        assert!(!guest.camera && guest.muted && !guest.speaking);
        assert_eq!(call.lobby.len(), 1);
        assert_eq!(call.lobby[0].safety, "11111");
        assert!(call.lobby[0].verified && !call.participants[1].verified);
    }

    #[test]
    fn a_knock_with_a_members_name_shows_its_key() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", true, now);
        let mut v = view(now);
        v.lobby[0].name = "host".into();
        let shown = apply(&v, &Seen::default(), |_| false, &mut call, now);
        let knock = PublicId([3; 32]);
        assert_eq!(call.lobby[0].name, format!("host ({})", short(&knock)));
        assert_eq!(shown[&knock], call.lobby[0].name, "chat uses the same name");
        assert!(call.participants[1].name.starts_with("Host ("));
    }

    #[test]
    fn someone_who_left_disappears_and_you_stay() {
        let now = Instant::now();
        let mut call = Call::live("Call".into(), "Jo", true, now);
        apply(&view(now), &Seen::default(), |_| false, &mut call, now);
        let mut later = view(now);
        later.people.remove(1);
        apply(&later, &Seen::default(), |_| false, &mut call, now);
        assert_eq!(call.participants.len(), 2);
        assert_eq!(call.you().map(|p| p.name.as_str()), Some("Jo"));
    }
}
