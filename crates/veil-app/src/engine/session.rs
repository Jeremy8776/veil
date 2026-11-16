//! The media side of a call, shared by host and guest: keys in, sealed media out and in.
//!
//! Purpose: [`Session`] holds the current epoch's frame sender and a [`KeyRing`] for
//! receiving. It seals each encoded picture (`FrameSender::encrypt_layer`, with the layer the
//! schedule chose) and each Opus frame, sends them through the `MediaSession`, and opens what
//! arrives: video goes to the decoder thread, sound to the playback thread. Keyframe requests
//! go both ways. [`Session::pump`] does one of these jobs per call, so the host and guest
//! loops can wait on it next to their own events.
//!
//! Invariants:
//! - Who sent a frame is its authenticated sender index (MLS leaf), never the link it came on.
//! - Nothing is sent before the first epoch's keys; nothing is encoded while you are alone.
//! - Full queues to the decoder and playback threads drop the packet; this task never blocks
//!   on them.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior, interval};
use veil_call::{EpochKeys, Incoming, KeyRing, Media, MediaSession, Path, Routes};
use veil_media::{AudioHeader, AudioSettings, OpusEncoder};
use veil_security::PublicId;
use veil_security::frame::{FrameSender, frame_layer};
use veil_security::safety::safety_number;

use super::audio_out::AudioIn;
use super::video_in::VideoIn;
use super::video_out::{Encoded, VideoControl};
use super::view::{Person, Shared};
use super::{Ids, micros_since, short};
use crate::model::LinkPath;

/// The channels between the session task and the media threads.
pub struct Pipes {
    pub encoded: mpsc::Receiver<Encoded>,
    pub sound: mpsc::Receiver<Vec<f32>>,
    /// Participant ids whose video cannot decode until their next keyframe.
    pub keyframes: mpsc::UnboundedReceiver<u32>,
    pub video_in: SyncSender<VideoIn>,
    pub audio_in: SyncSender<AudioIn>,
    pub control: Arc<VideoControl>,
}

pub struct Session {
    pub media: MediaSession,
    me: PublicId,
    host: PublicId,
    pub ids: Ids,
    sender: Option<FrameSender>,
    ring: Option<KeyRing>,
    /// This epoch's leaf index to participant id.
    leaves: HashMap<u32, u32>,
    /// This epoch's other members, in roster order.
    others: Vec<(u32, PublicId)>,
    opus: Option<OpusEncoder>,
    audio_seq: u32,
    frame_id: u32,
    clock: Instant,
    tick: Interval,
    shared: Arc<Shared>,
    pipes: Pipes,
}

impl Session {
    pub fn new(
        media: MediaSession,
        me: PublicId,
        host: PublicId,
        shared: Arc<Shared>,
        pipes: Pipes,
    ) -> Self {
        let mut tick = interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            media,
            me,
            host,
            ids: Ids::default(),
            sender: None,
            ring: None,
            leaves: HashMap::new(),
            others: Vec::new(),
            opus: None,
            audio_seq: 0,
            frame_id: 0,
            clock: Instant::now(),
            tick,
            shared,
            pipes,
        }
    }

    /// How someone is named until names are sent: the host, or a guest by key.
    // shortcut: names (and chat) are not sent between members. Add a sealed app message to
    // `veil-call` and show the name each member chose.
    pub fn name_for(&self, device: &PublicId) -> String {
        if *device == self.host {
            String::from("Host")
        } else {
            format!("Guest {}", short(device))
        }
    }

    pub fn safety_with(&self, device: &PublicId) -> String {
        safety_number(&self.me, device)
    }

    /// A new epoch: new keys, and the people list follows the roster.
    pub fn on_keys(&mut self, keys: EpochKeys) {
        match &mut self.ring {
            Some(ring) => ring.rotate(keys.epoch, keys.receiver, Instant::now()),
            None => self.ring = Some(KeyRing::new(keys.epoch, keys.receiver)),
        }
        self.sender = Some(keys.sender);
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
        self.pipes
            .control
            .watching
            .store(watching, Ordering::Relaxed);
        let mut people: Vec<Person> = self
            .others
            .iter()
            .map(|(id, device)| Person {
                id: *id,
                name: self.name_for(device),
                is_host: *device == self.host,
                safety: self.safety_with(device),
                path: LinkPath::Pending,
            })
            .collect();
        let epoch = keys.epoch;
        self.shared.update(|view| {
            // Links that already exist stay up across epochs; `refresh_paths` corrects the rest.
            for person in &mut people {
                if let Some(known) = view.people.iter().find(|o| o.id == person.id) {
                    person.path = known.path;
                }
            }
            view.people = people;
            view.epoch = epoch;
        });
    }

    /// Links up with the members this epoch's plan names.
    pub async fn on_routes(&mut self, routes: Routes) {
        for (peer, e) in self.media.apply(routes).await {
            eprintln!("veil-app: cannot reach {}: {e}", short(&peer));
        }
        // Whoever just linked up needs a keyframe to start decoding.
        self.pipes.control.keyframe.store(true, Ordering::Relaxed);
        self.refresh_paths();
    }

    /// Does one job: media in, media out, a keyframe request, or the once-a-second refresh.
    pub async fn pump(&mut self) {
        tokio::select! {
            Some(incoming) = self.media.recv() => self.on_incoming(&incoming),
            Some(encoded) = self.pipes.encoded.recv() => self.send_video(encoded).await,
            Some(pcm) = self.pipes.sound.recv() => self.send_audio(&pcm),
            Some(id) = self.pipes.keyframes.recv() => self.ask_keyframe(id).await,
            _ = self.tick.tick() => self.refresh_paths(),
        }
    }

    fn refresh_paths(&self) {
        let paths: HashMap<PublicId, Path> = self.media.paths().into_iter().collect();
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

    async fn send_video(&mut self, encoded: Encoded) {
        if self.others.is_empty() {
            return;
        }
        let Some(sender) = &mut self.sender else {
            return;
        };
        match sender.encrypt_layer(encoded.layer, &encoded.payload) {
            Ok(sealed) => {
                self.frame_id = self.frame_id.wrapping_add(1);
                self.media.send_frame(self.frame_id, &sealed).await;
            }
            Err(e) => eprintln!("veil-app: cannot seal a picture: {e}"),
        }
    }

    fn send_audio(&mut self, pcm: &[f32]) {
        if self.others.is_empty() {
            return;
        }
        let Some(sender) = &mut self.sender else {
            return;
        };
        if self.opus.is_none() {
            match OpusEncoder::new(&AudioSettings::voice()) {
                Ok(encoder) => self.opus = Some(encoder),
                Err(e) => {
                    eprintln!("veil-app: no audio encoder: {e}");
                    return;
                }
            }
        }
        let Some(opus) = &mut self.opus else { return };
        if pcm.len() != opus.frame_len() {
            return;
        }
        let Ok(packet) = opus.encode(pcm) else { return };
        self.audio_seq = self.audio_seq.wrapping_add(1);
        let header = AudioHeader {
            sequence: self.audio_seq,
            timestamp_us: micros_since(self.clock),
        };
        let Ok(payload) = header.write(&packet) else {
            return;
        };
        match sender.encrypt(&payload) {
            Ok(sealed) => {
                self.media.send_audio(&sealed);
            }
            Err(e) => eprintln!("veil-app: cannot seal sound: {e}"),
        }
    }

    fn on_incoming(&mut self, incoming: &Incoming) {
        let (packet, video) = match &incoming.media {
            Media::KeyframeRequest => {
                self.pipes.control.keyframe.store(true, Ordering::Relaxed);
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
        let Ok((leaf, plain)) = ring.open(packet, Instant::now()) else {
            return;
        };
        let Some(&id) = self.leaves.get(&leaf) else {
            return;
        };
        if let Some(layer) = layer {
            let _ = self.pipes.video_in.try_send(VideoIn {
                id,
                layer,
                payload: plain,
            });
            return;
        }
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

    async fn ask_keyframe(&mut self, id: u32) {
        if let Some(device) = self.ids.device(id) {
            let _ = self.media.request_keyframe(device).await;
        }
    }
}

fn link_path(path: &Path) -> LinkPath {
    match path {
        Path::Direct => LinkPath::Direct,
        Path::Punched => LinkPath::Punched,
        Path::Relayed => LinkPath::Relayed,
        Path::Forwarder => LinkPath::Forwarder,
    }
}
