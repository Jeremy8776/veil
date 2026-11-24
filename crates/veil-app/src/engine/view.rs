//! What the engine knows about the call, shared between its threads and the interface.
//!
//! Purpose: the session task writes [`View`] (status, invite, people, lobby, key epoch) and
//! queues chat and reactions that arrived, the decoder thread writes pictures and the
//! playback thread writes what it heard. [`apply`] turns all of that into the screens'
//! [`Call`], once per interface frame, and [`Shared::take_said`] hands over new chat once.
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
use veil_security::safety::Side;

use super::names::labels;
use super::stats::{Decoded, Stats};
use crate::convert::Rgba;
use crate::media::lock;
use crate::model::{Call, CallStatus, LinkPath, Participant, Reachable, RouterPort, Waiting};

/// Chat messages and reactions that may wait for the interface; more drop the oldest.
const CHAT_QUEUE: usize = 256;
/// No sound for this long shows someone as muted.
const QUIET: Duration = Duration::from_secs(1);
/// No picture for this long shows someone's camera as off.
const DARK: Duration = Duration::from_secs(2);
/// A level above this, on the meter's 0 to 1 scale, shows someone speaking.
pub const SPEAKING: f32 = 0.45;
/// The speaking border stays on this long after the last loud frame, so it follows speech
/// rather than flickering between syllables.
pub const SPEAKING_HOLD: Duration = Duration::from_millis(300);

/// Whether someone whose level last passed [`SPEAKING`] at `loud_at` still shows speaking.
pub fn still_speaking(loud_at: Option<Instant>, now: Instant) -> bool {
    loud_at.is_some_and(|at| now.saturating_duration_since(at) < SPEAKING_HOLD)
}

/// Chat that arrived, for the interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Said {
    /// A message; `id` is None from 0.1.11 and earlier.
    Chat {
        id: Option<u64>,
        text: String,
    },
    React {
        id: u64,
        emoji: String,
        on: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Person {
    pub id: u32,
    pub device: PublicId,
    pub name: String,
    pub is_host: bool,
    pub safety: String,
    pub side: Side,
    pub path: LinkPath,
    /// The hand state this person last sent.
    pub hand_raised: bool,
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
    /// See `Call::invite_full`.
    pub invite_full: String,
    /// See `Call::nearby_code`.
    pub nearby_code: String,
    /// See `Call::invite_expires` and `Call::invite_until_end`.
    pub invite_expires: u64,
    pub invite_until_end: bool,
    pub epoch: u64,
    /// See `Call::group`.
    pub group: String,
    /// Everyone but you, in roster order.
    pub people: Vec<Person>,
    pub lobby: Vec<Knocker>,
    pub router: RouterPort,
    pub reach: Reachable,
    /// See `Call::server_down`.
    pub server_down: bool,
}

/// Marks a picture key as someone's shared screen rather than their camera.
const SCREEN_KEY: u32 = 1 << 31;

/// The key of person `id`'s shared screen, among the pictures and the interface's textures.
pub fn screen_key(id: u32) -> u32 {
    id | SCREEN_KEY
}

pub fn is_screen(key: u32) -> bool {
    key & SCREEN_KEY != 0
}

/// The newest picture from one person. `rgba` is taken by the interface once uploaded.
struct Picture {
    rgba: Option<Rgba>,
    at: Instant,
    /// Decoded size, before any halving for the screen, and pictures decoded so far.
    decoded: Decoded,
}

/// What the playback thread heard from one person.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    /// When the last packet arrived.
    pub at: Instant,
    /// Level of the last frame played, 0 to 1.
    pub level: f32,
    /// When their level last passed [`SPEAKING`].
    pub loud_at: Option<Instant>,
}

/// When each person last showed video and sound. `screen` is keyed by the sharer's own id.
#[derive(Default)]
pub struct Seen {
    pub video: HashMap<u32, Instant>,
    pub screen: HashMap<u32, Instant>,
    pub audio: HashMap<u32, Heard>,
}

pub struct Shared {
    pub diagnostic: Mutex<crate::diagnostics::Report>,
    pub view: Mutex<View>,
    pictures: Mutex<HashMap<u32, Picture>>,
    heard: Mutex<HashMap<u32, Heard>>,
    /// Chat that arrived, with the sender's device, not yet in the interface's call.
    chat: Mutex<Vec<(PublicId, Said)>>,
    /// Each other person's tile on screen, as (participant id, width in physical pixels).
    tiles: Mutex<Vec<(u32, f32)>>,
    /// The newest call stats, for the debug overlay (`stats.rs`).
    stats: Mutex<Stats>,
    ctx: egui::Context,
}

impl Shared {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            diagnostic: Mutex::default(),
            view: Mutex::new(View {
                status: CallStatus::Connecting,
                invite: String::new(),
                invite_full: String::new(),
                nearby_code: String::new(),
                invite_expires: 0,
                invite_until_end: false,
                epoch: 0,
                group: String::new(),
                people: Vec::new(),
                lobby: Vec::new(),
                router: RouterPort::Off,
                reach: Reachable::Unknown,
                server_down: false,
            }),
            pictures: Mutex::default(),
            heard: Mutex::default(),
            chat: Mutex::default(),
            tiles: Mutex::default(),
            stats: Mutex::default(),
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

    pub fn diagnose(&self, change: impl FnOnce(&mut crate::diagnostics::Report)) {
        change(&mut lock(&self.diagnostic));
        self.ctx.request_repaint();
    }

    /// The newest picture from `id`, decoded at `size` (width, height).
    pub fn put_picture(&self, id: u32, rgba: Rgba, size: (u16, u16)) {
        let mut pictures = lock(&self.pictures);
        let frames = pictures.get(&id).map_or(0, |p| p.decoded.frames);
        pictures.insert(
            id,
            Picture {
                rgba: Some(rgba),
                at: Instant::now(),
                decoded: Decoded {
                    width: size.0,
                    height: size.1,
                    frames: frames.saturating_add(1),
                },
            },
        );
        drop(pictures);
        self.ctx.request_repaint();
    }

    /// Each person's decoded video so far, for the stats.
    pub fn decoded(&self) -> HashMap<u32, Decoded> {
        lock(&self.pictures)
            .iter()
            .filter(|(id, _)| !is_screen(**id))
            .map(|(id, p)| (*id, p.decoded))
            .collect()
    }

    pub fn set_stats(&self, stats: Stats) {
        *lock(&self.stats) = stats;
    }

    pub fn stats(&self) -> Stats {
        lock(&self.stats).clone()
    }

    pub fn forget_picture(&self, id: u32) {
        lock(&self.pictures).remove(&id);
        self.ctx.request_repaint();
    }

    /// Who is sharing their screen now, lowest id first.
    pub fn sharers(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = lock(&self.pictures)
            .keys()
            .filter(|key| is_screen(**key))
            .map(|key| key & !SCREEN_KEY)
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn heard(&self, id: u32, change: impl FnOnce(&mut Heard)) {
        let mut heard = lock(&self.heard);
        let entry = heard.entry(id).or_insert(Heard {
            at: Instant::now(),
            level: 0.0,
            loud_at: None,
        });
        change(entry);
    }

    pub fn forget_heard(&self, id: u32) {
        lock(&self.heard).remove(&id);
    }

    /// Queues a chat message or reaction that arrived for the interface.
    pub fn push_said(&self, from: PublicId, said: Said) {
        let mut chat = lock(&self.chat);
        if chat.len() >= CHAT_QUEUE {
            chat.remove(0);
        }
        chat.push((from, said));
        drop(chat);
        self.ctx.request_repaint();
    }

    /// Chat and reactions that arrived since the last call.
    pub fn take_said(&self) -> Vec<(PublicId, Said)> {
        std::mem::take(&mut *lock(&self.chat))
    }

    pub fn take_pictures(&self) -> Vec<(u32, Rgba)> {
        lock(&self.pictures)
            .iter_mut()
            .filter_map(|(id, p)| p.rgba.take().map(|rgba| (*id, rgba)))
            .collect()
    }

    pub fn seen(&self) -> Seen {
        let mut seen = Seen {
            audio: lock(&self.heard).clone(),
            ..Seen::default()
        };
        for (id, picture) in lock(&self.pictures).iter() {
            if is_screen(*id) {
                seen.screen.insert(*id & !SCREEN_KEY, picture.at);
            } else {
                seen.video.insert(*id, picture.at);
            }
        }
        seen
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
    call.invite_full.clone_from(&view.invite_full);
    call.nearby_code.clone_from(&view.nearby_code);
    call.invite_expires = view.invite_expires;
    call.invite_until_end = view.invite_until_end;
    call.router.clone_from(&view.router);
    call.reach.clone_from(&view.reach);
    call.server_down = view.server_down;
    call.epoch = view.epoch;
    call.group.clone_from(&view.group);
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
            hand_raised: person.hand_raised,
            speaking: heard.is_some_and(|h| still_speaking(h.loud_at, now)),
            path: person.path,
            safety: person.safety.clone(),
            side: person.side,
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
            invite_full: "veil://join/x".into(),
            nearby_code: String::new(),
            invite_expires: 0,
            invite_until_end: false,
            epoch: 3,
            group: String::from("24680"),
            people: vec![
                Person {
                    id: 1,
                    device: PublicId([1; 32]),
                    name: "Host".into(),
                    is_host: true,
                    safety: "12345".into(),
                    side: Side::A,
                    path: LinkPath::Direct,
                    hand_raised: true,
                },
                Person {
                    id: 2,
                    device: PublicId([2; 32]),
                    name: "Guest ab12cd34".into(),
                    is_host: false,
                    safety: "67890".into(),
                    side: Side::B,
                    path: LinkPath::Pending,
                    hand_raised: false,
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
            server_down: false,
        }
    }

    #[test]
    fn the_speaking_border_holds_between_syllables() {
        let now = Instant::now();
        assert!(!still_speaking(None, now));
        assert!(still_speaking(Some(now), now));
        let gap = now + Duration::from_millis(200);
        assert!(
            still_speaking(Some(now), gap),
            "a short gap keeps the border"
        );
        let pause = now + SPEAKING_HOLD;
        assert!(!still_speaking(Some(now), pause), "a pause ends it");
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
                loud_at: Some(now),
            },
        );
        seen.audio.insert(
            2,
            Heard {
                at: now.checked_sub(Duration::from_secs(5)).unwrap_or(now),
                level: 0.9,
                loud_at: Some(now),
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
        assert_eq!(call.group, "24680");
        assert_eq!(call.participants.len(), 3);
        assert!(call.participants[0].is_you);
        let host = &call.participants[1];
        assert!(host.camera && !host.muted && host.speaking && host.is_host);
        let guest = &call.participants[2];
        assert!(!guest.camera && guest.muted && !guest.speaking);
        assert!(
            host.hand_raised && !guest.hand_raised,
            "each hand as that person sent it"
        );
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
