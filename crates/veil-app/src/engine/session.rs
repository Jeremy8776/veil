//! The media side of a call, shared by host and guest: keys in, sealed media out and in.
//!
//! Purpose: [`Session`] holds the current epoch's frame sender and a [`KeyRing`] for
//! receiving. It seals each encoded picture (`FrameSender::encrypt_layer`, with the layer the
//! schedule chose) and each Opus frame, sends them through the `MediaSession`, and opens what
//! arrives: video goes to the decoder thread, sound to the playback thread. Keyframe requests
//! go both ways. Your microphone and your share's computer sound are separate streams
//! (`sound_out.rs`). Names, chat and reactions go both ways as sealed app messages (`names.rs` decides when
//! your name is sent); received chat waits in [`Shared`] for the interface. Through a
//! forwarder it asks for the layers each tile needs (`wanted.rs`). [`Session::pump`]
//! does one of these jobs per call, so the host and guest loops can wait on it next to their
//! own events.
//!
//! Invariants:
//! - Who sent a frame or message is its authenticated sender index (MLS leaf), never the link
//!   it came on. A shared screen, and its computer sound, also need that leaf's device
//!   signature (`screen_proof.rs`, `sound_proof.rs`).
//! - Nothing is sent before the first epoch's keys; nothing is encoded while you are alone.
//! - Full queues to the decoder and playback threads drop the packet; this task never blocks
//!   on them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior, interval};
use veil_call::{
    AppError, AppMessage, EpochKeys, Incoming, KeyRing, LinkEvent, Media, MediaSession, Path,
    Routes,
};
use veil_forward::Wanted;
use veil_media::AudioHeader;
use veil_security::frame::{FrameSender, frame_layer};
use veil_security::group_code::group_number;
use veil_security::safety::{safety_number, side};
use veil_security::{Identity, PublicId};

use super::audio_out::AudioIn;
use super::auto_step::AutoStep;
use super::names::{Announcer, Names};
use super::screen_proof;
use super::sound_out::SoundOut;
use super::sound_proof;
use super::stats::{Sampler, link_path};
use super::video_in::VideoIn;
use super::video_out::{Encoded, VideoControl};
use super::view::{Person, Said, Shared};
use super::wanted;
use super::{Ids, micros_since, short};
use crate::data_use::DataUseCell;
use crate::model::{Action, LinkPath};

mod moderate;
/// The channels between the session task and the media threads.
pub struct Pipes {
    pub encoded: mpsc::Receiver<Encoded>,
    pub sound: mpsc::Receiver<Vec<f32>>,
    /// Your share's computer sound, 20 ms of `AudioSettings::share_sound` at a time.
    pub share_sound: mpsc::Receiver<Vec<f32>>,
    /// Participant ids whose video cannot decode until their next keyframe.
    pub keyframes: mpsc::Receiver<u32>,
    pub video_in: SyncSender<VideoIn>,
    pub audio_in: SyncSender<AudioIn>,
    pub control: Arc<VideoControl>,
    pub screen_control: Arc<VideoControl>,
    /// The data use choice, read for each sound frame and each layer request.
    pub data: Arc<DataUseCell>,
}

pub(super) fn diagnose_member(shared: &Shared, event: &veil_call::MemberEvent) {
    use crate::diagnostics::{End, Problem, State, Step};
    use veil_call::MemberEvent;
    if matches!(
        event,
        MemberEvent::Closed
            | MemberEvent::Removed
            | MemberEvent::ConnectionLost
            | MemberEvent::Overloaded
            | MemberEvent::Failed(_)
    ) {
        shared.diagnose(|d| d.reconnecting = false);
    }
    shared.diagnose(|d| match event {
        MemberEvent::Reconnecting => d.reconnecting = true,
        MemberEvent::Reconnected => d.reconnecting = false,
        MemberEvent::Keys(_) => d.set(Step::Membership, State::Passed),
        MemberEvent::Closed => d.end = Some(End::HostEnded),
        MemberEvent::Removed => {
            d.end = Some(End::Removed);
            d.set(Step::Membership, State::Failed(Problem::Removed));
        }
        MemberEvent::ConnectionLost => {
            d.end = Some(End::Failed);
            d.set(Step::HostConnection, State::Failed(Problem::Recovery));
        }
        MemberEvent::Overloaded => {
            d.end = Some(End::Failed);
            d.set(Step::Membership, State::Failed(Problem::Overloaded));
        }
        MemberEvent::Failed(_) => {
            d.end = Some(End::Failed);
            d.set(Step::Membership, State::Failed(Problem::Other));
        }
        MemberEvent::Routes(_) | MemberEvent::PlanRefused(_) => {}
    });
}

impl Pipes {
    fn controls(&self) -> [&VideoControl; 2] {
        [&self.control, &self.screen_control]
    }

    /// A keyframe request does not say which stream broke, so both answer.
    fn ask_keyframes(&self) {
        for control in self.controls() {
            control.keyframe.store(true, Ordering::Relaxed);
        }
    }
}

pub struct Session {
    pub media: MediaSession,
    /// This device's key, which signs every name, hand and chat message it sends.
    identity: Identity,
    me: PublicId,
    host: PublicId,
    pub ids: Ids,
    sender: Option<FrameSender>,
    ring: Option<KeyRing>,
    /// This epoch's leaf index to participant id.
    leaves: HashMap<u32, u32>,
    /// This epoch's other members, in roster order.
    others: Vec<(u32, PublicId)>,
    names: Names,
    announcer: Announcer,
    /// The layer request last sent to the forwarder; `None` after new routes, so it is sent
    /// again (a new forwarder link starts with every layer).
    subscribed: Option<Vec<(PublicId, Wanted)>>,
    /// Turns link counters into the debug overlay's rates, once a second.
    sampler: Sampler,
    /// Whether Auto data use should save data, judged from the same link rows.
    auto: AutoStep,
    /// Your microphone and your share's computer sound, each a stream of its own.
    voice: SoundOut,
    share_sound: SoundOut,
    frame_id: u32,
    clock: Instant,
    tick: Interval,
    shared: Arc<Shared>,
    pipes: Pipes,
    /// Who moderates, and the checks on moderation messages (`session/moderate.rs`).
    roles: crate::engine::moderation::Roles,
    /// Host: moderators' checked requests, until `host.rs` carries them out.
    requests: Vec<veil_call::Moderation>,
}

impl Session {
    pub fn evidence(&self) -> Arc<Shared> {
        self.shared.clone()
    }
    pub fn diagnose_member(&self, event: &veil_call::MemberEvent) {
        diagnose_member(&self.shared, event);
    }
    pub fn new(
        media: MediaSession,
        identity: &Identity,
        host: PublicId,
        your_name: &str,
        shared: Arc<Shared>,
        pipes: Pipes,
    ) -> Self {
        let mut tick = interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            media,
            identity: Identity::from_secret(&identity.secret_bytes()),
            me: identity.public(),
            host,
            ids: Ids::default(),
            sender: None,
            ring: None,
            leaves: HashMap::new(),
            others: Vec::new(),
            names: Names::default(),
            announcer: Announcer::new(your_name),
            subscribed: None,
            sampler: Sampler::default(),
            auto: AutoStep::default(),
            voice: SoundOut::voice(),
            share_sound: SoundOut::share(),
            frame_id: 0,
            clock: Instant::now(),
            tick,
            shared,
            pipes,
            roles: Self::roles(host, identity.public()),
            requests: Vec::new(),
        }
    }

    /// The name `device` sent, or "Host" or "Guest" and a key prefix until it does.
    pub fn name_for(&self, device: &PublicId) -> String {
        self.names.of(device, &self.host)
    }

    pub fn safety_with(&self, device: &PublicId) -> String {
        safety_number(&self.me, device)
    }

    /// A new epoch: new keys, and the people list follows the roster.
    pub fn on_keys(&mut self, keys: EpochKeys) {
        self.media.members(keys.roster.iter().map(|(_, d)| *d));
        let roster = keys.roster.iter().copied();
        match &mut self.ring {
            Some(ring) => ring.rotate(keys.epoch, keys.receiver, roster, Instant::now()),
            None => self.ring = Some(KeyRing::new(keys.epoch, keys.receiver, roster)),
        }
        self.sender = Some(keys.sender);
        self.announcer.restart(Instant::now());
        self.leaves.clear();
        self.others.clear();
        for (leaf, device) in &keys.roster {
            if *device != self.me {
                let id = self.ids.id(*device);
                self.leaves.insert(*leaf, id);
                self.others.push((id, *device));
            }
        }
        let watching = !self.others.is_empty();
        for control in self.pipes.controls() {
            control.watching.store(watching, Ordering::Relaxed);
        }
        let mut people: Vec<Person> = self
            .others
            .iter()
            .map(|(id, device)| Person {
                id: *id,
                device: *device,
                name: self.name_for(device),
                is_host: *device == self.host,
                safety: self.safety_with(device),
                side: side(&self.me, device),
                path: LinkPath::Pending,
                hand_raised: self.names.hand(device),
            })
            .collect();
        let epoch = keys.epoch;
        let devices: Vec<PublicId> = keys.roster.iter().map(|(_, d)| *d).collect();
        let group = group_number(&devices);
        self.roles_epoch(&devices);
        self.shared.update(|view| {
            // Links that already exist stay up across epochs; `refresh_paths` corrects the rest.
            for person in &mut people {
                if let Some(known) = view.people.iter().find(|o| o.id == person.id) {
                    person.path = known.path;
                }
            }
            view.people = people;
            view.epoch = epoch;
            view.group = group;
        });
    }

    /// Links up with the members this epoch's plan names.
    pub fn reconnecting(&self, recovering: bool) {
        self.shared.update(|view| {
            view.status = if recovering {
                crate::model::CallStatus::Reconnecting
            } else {
                crate::model::CallStatus::Live
            }
        });
    }

    pub async fn on_routes(&mut self, routes: Routes) {
        let started = Instant::now();
        for (peer, e) in self.media.apply(routes).await {
            crate::note!("cannot reach {}: {e}", short(&peer));
        }
        // Dials hold the host's call loop (shortcut in host.rs), so leaves and chat wait there.
        // A guest still hears the call end and can leave (`guest::link_up`). Media waits on both.
        if started.elapsed() > Duration::from_secs(1) {
            crate::note!("call: linking up took {:?}", started.elapsed());
        }
        // Whoever just linked up needs a keyframe to start decoding.
        self.pipes.ask_keyframes();
        self.announcer.restart(Instant::now());
        self.refresh_paths();
        self.announce().await;
        self.subscribed = None;
        self.subscribe().await;
    }

    /// Asks the forwarder for the layers the tiles on screen need, if that changed. Does
    /// nothing in a mesh. At most once a second (the tick), so a quick change of speaker
    /// does not ask senders for a keyframe each time.
    async fn subscribe(&mut self) {
        let cap = self.pipes.data.effective().wanted_cap();
        let request = wanted::request(&self.others, &self.shared.tiles(), cap);
        if request.is_empty() || self.subscribed.as_ref() == Some(&request) {
            return;
        }
        match self.media.subscribe(&request).await {
            Ok(()) => self.subscribed = Some(request),
            Err(e) => crate::note!("cannot ask for layers: {e}"),
        }
    }

    /// Does one job: media in, media out, a keyframe request, or the once-a-second refresh.
    pub async fn pump(&mut self) {
        tokio::select! {
            Some(incoming) = self.media.recv() => self.on_incoming(&incoming),
            Some(encoded) = self.pipes.encoded.recv() => self.send_video(encoded).await,
            Some(pcm) = self.pipes.sound.recv() => self.send_sound(&pcm, false),
            Some(pcm) = self.pipes.share_sound.recv() => self.send_sound(&pcm, true),
            Some(id) = self.pipes.keyframes.recv() => self.ask_keyframe(id).await,
            _ = self.tick.tick() => {
                self.media.redial();
                self.note_link_events();
                self.refresh_paths();
                self.sample_stats();
                self.announce().await;
                self.subscribe().await;
            }
        }
    }

    /// Logs media links that dropped, came back, or could not be dialled again.
    fn note_link_events(&mut self) {
        for event in self.media.take_events() {
            match event {
                LinkEvent::Dropped(peer) => crate::note!("call: lost the link to {}", short(&peer)),
                LinkEvent::Back { peer, after } => {
                    crate::note!("call: linked to {} again after {after:.1?}", short(&peer));
                    // A fresh forwarder link has no subscriptions, and the peer may have
                    // missed our last keyframe, name or hand state during the outage.
                    self.subscribed = None;
                    self.pipes.ask_keyframes();
                    self.announcer.restart(Instant::now());
                }
                LinkEvent::StillDown(peer, e) => {
                    crate::note!("call: cannot reach {} yet, retrying: {e}", short(&peer));
                }
            }
        }
    }

    /// Sends a chat message or a reaction to everyone; other actions are not chat and are
    /// ignored. Nothing goes out while you are alone.
    pub async fn say(&mut self, action: &Action) {
        let message = match action {
            Action::Chat(id, text) => AppMessage::chat(*id, text),
            Action::React(id, emoji, on) => AppMessage::react(*id, emoji, *on),
            _ => return,
        };
        match message {
            Ok(message) => self.send_app(&message).await,
            Err(e) => crate::note!("chat not sent: {e}"),
        }
    }

    /// Raises or lowers your hand for the others. Sent now and again for a few seconds.
    pub async fn set_hand(&mut self, raised: bool) {
        self.announcer.set_hand(raised, Instant::now());
        self.announce().await;
    }

    /// Sends your name and hand if the announcer says it is time.
    async fn announce(&mut self) {
        for message in self.announcer.due(Instant::now()) {
            self.send_app(&message).await;
        }
        self.send_moderation().await;
    }

    async fn send_app(&mut self, message: &AppMessage) {
        if self.others.is_empty() {
            return;
        }
        let Some(sender) = &mut self.sender else {
            return;
        };
        match message.seal(sender, &self.identity) {
            Ok(sealed) => {
                self.media.send_app(&sealed).await;
            }
            Err(e) => crate::note!("cannot seal a message: {e}"),
        }
    }

    fn refresh_paths(&mut self) {
        let paths: HashMap<PublicId, Path> = self.media.paths().into_iter().collect();
        let linked: HashSet<PublicId> = paths.keys().copied().collect();
        self.announcer.links(linked, Instant::now());
        let by_id: HashMap<u32, LinkPath> = self
            .others
            .iter()
            .map(|(id, device)| (*id, paths.get(device).map_or(LinkPath::Pending, link_path)))
            .collect();
        let mut view = crate::media::lock(&self.shared.view);
        for person in &mut view.people {
            if let Some(path) = by_id.get(&person.id) {
                person.path = *path;
            }
        }
    }

    /// Measures each link and each person's video for the debug overlay, and lets Auto data
    /// use judge the links. Reads counters only.
    fn sample_stats(&mut self) {
        let links = self.media.link_stats();
        let decoded = self.shared.decoded();
        let sent = *crate::media::lock(&self.pipes.screen_control.sent);
        let others = &self.others;
        let person =
            |device: &PublicId| others.iter().find(|(_, d)| d == device).map(|(id, _)| *id);
        let stats = self
            .sampler
            .sample(links, &decoded, sent, person, Instant::now());
        self.pipes
            .data
            .set_auto_saving(self.auto.update(&stats.links));
        self.shared.set_stats(stats);
    }

    async fn send_video(&mut self, encoded: Encoded) {
        if self.others.is_empty() {
            return;
        }
        let Some(sender) = &mut self.sender else {
            return;
        };
        let signed;
        let payload = if screen_proof::is_screen(&encoded.payload) {
            let (binding, id) = (sender.binding(), sender.next_frame_id());
            let proof = screen_proof::sign(
                &self.identity,
                binding,
                &id,
                encoded.layer,
                &encoded.payload,
            );
            let Some(proof) = proof else { return };
            signed = proof;
            signed.as_slice()
        } else {
            encoded.payload.as_slice()
        };
        match sender.encrypt_layer(encoded.layer, payload) {
            Ok(sealed) => {
                self.frame_id = self.frame_id.wrapping_add(1);
                self.media.send_frame(self.frame_id, &sealed).await;
            }
            Err(e) => crate::note!("cannot seal a picture: {e}"),
        }
    }

    /// Seals and sends one frame of your microphone, or of your share's computer sound.
    fn send_sound(&mut self, pcm: &[f32], share: bool) {
        if self.others.is_empty() {
            return;
        }
        let Some(sender) = &mut self.sender else {
            return;
        };
        let timestamp = micros_since(self.clock);
        let data = self.pipes.data.effective();
        let payload = if share {
            let payload =
                self.share_sound
                    .payload(pcm, Some(data.share_sound_bitrate()), timestamp);
            let (binding, id) = (sender.binding(), sender.next_frame_id());
            payload.and_then(|p| sound_proof::sign(&self.identity, binding, &id, &p))
        } else {
            self.voice
                .payload(pcm, Some(data.opus_bitrate()), timestamp)
        };
        let Some(payload) = payload else { return };
        match sender.encrypt(&payload) {
            Ok(sealed) => {
                self.media.send_audio(&sealed);
            }
            Err(e) => crate::note!("cannot seal sound: {e}"),
        }
    }

    fn on_incoming(&mut self, incoming: &Incoming) {
        let (packet, video) = match &incoming.media {
            Media::KeyframeRequest => {
                self.pipes.ask_keyframes();
                return;
            }
            Media::App(sealed) => {
                self.on_app(sealed, &incoming.via);
                return;
            }
            Media::Video { sealed, .. } => (sealed.as_slice(), true),
            Media::Audio(bytes) => (bytes.as_ref(), false),
        };
        let Some(ring) = &mut self.ring else { return };
        let layer = if video {
            match frame_layer(packet) {
                Ok(layer) => Some(layer),
                Err(_) => return,
            }
        } else {
            None
        };
        // A member's link may carry only that member's frames (GHSA-rvhv-r5mr-2w3h).
        let Ok((leaf, plain)) = ring.open_from(packet, &incoming.via, Instant::now()) else {
            return;
        };
        let Some(&id) = self.leaves.get(&leaf) else {
            return;
        };
        if let Some(layer) = layer {
            // A shared screen must carry the signature of the device that holds this leaf.
            let payload = if screen_proof::is_screen(&plain) {
                let Some((device, binding)) = ring.signer(packet, leaf, Instant::now()) else {
                    return;
                };
                match screen_proof::check(&device, &binding, packet, layer, plain) {
                    Some(payload) => payload,
                    None => return,
                }
            } else {
                plain
            };
            let _ = self.pipes.video_in.try_send(VideoIn { id, layer, payload });
            return;
        }
        // Share sound must carry the signature of the device that holds this leaf.
        let plain = if sound_proof::is_share_sound(&plain) {
            let Some((device, binding)) = ring.signer(packet, leaf, Instant::now()) else {
                return;
            };
            match sound_proof::check(&device, &binding, packet, plain) {
                Some(plain) => plain,
                None => return,
            }
        } else {
            plain
        };
        let Ok((header, opus)) = AudioHeader::read(&plain) else {
            return;
        };
        let _ = self.pipes.audio_in.try_send(AudioIn {
            id,
            header,
            packet: opus.to_vec(),
            arrival_us: micros_since(self.clock),
        });
    }

    /// Opens a name, hand or chat message and shows it. Anything that does not open, comes from a
    /// leaf outside its epoch, is not signed by that leaf's device or fails the checks is
    /// dropped.
    fn on_app(&mut self, sealed: &[u8], via: &PublicId) {
        let Some(ring) = &mut self.ring else { return };
        let (device, message) = match ring.open_app(sealed, via, Instant::now()) {
            Ok(opened) => opened,
            Err(AppError::Unopened) => return,
            Err(e) => {
                crate::note!("dropped a message via {}: {e}", short(via));
                return;
            }
        };
        let Some(&(id, _)) = self.others.iter().find(|(_, d)| *d == device) else {
            return;
        };
        match message {
            AppMessage::Name(name) => {
                if self.names.set(device, name.clone()) {
                    self.shared.update(|view| {
                        for person in view.people.iter_mut().filter(|p| p.id == id) {
                            person.name.clone_from(&name);
                        }
                    });
                }
            }
            AppMessage::Hand(raised) => {
                if self.names.set_hand(device, raised) {
                    self.shared.update(|view| {
                        for person in view.people.iter_mut().filter(|p| p.id == id) {
                            person.hand_raised = raised;
                        }
                    });
                }
            }
            AppMessage::Chat(text) => self.shared.push_said(device, Said::Chat { id: None, text }),
            AppMessage::Post { id, text } => {
                self.shared
                    .push_said(device, Said::Chat { id: Some(id), text });
            }
            AppMessage::React { id, emoji, on } => {
                self.shared.push_said(device, Said::React { id, emoji, on });
            }
            AppMessage::Mute(_)
            | AppMessage::Moderators(_)
            | AppMessage::Waiting(_)
            | AppMessage::Moderate(_) => self.on_moderation(device, &message),
        }
    }

    async fn ask_keyframe(&mut self, id: u32) {
        if let Some(device) = self.ids.device(id) {
            let _ = self.media.request_keyframe(device).await;
        }
    }
}
