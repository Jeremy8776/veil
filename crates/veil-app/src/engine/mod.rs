//! The real call: `veil-call` for links, the lobby and keys, `veil-media` for sound and video.
//!
//! Purpose: [`Engine`] runs one call away from the interface thread and shows it through
//! [`Engine::sync`], which copies what the engine knows into the screens' [`Call`]. The parts:
//! - a Tokio runtime running the session task: `host.rs` or `guest.rs` for the call itself,
//!   `session.rs` for sealing, sending, receiving and opening media, `plan.rs` for addresses
//!   and the host's layout of each key epoch;
//! - `video_out.rs`, a thread that encodes your camera (H.264 simulcast);
//! - `video_in.rs`, a thread that checks and decodes everyone else's video;
//! - `audio_out.rs`, a thread that orders, decodes, mixes and plays everyone's sound, and the
//!   computer sound of their shares.
//!
//! Your camera and microphone threads (`media.rs`) feed the engine through [`Taps`].
//! Every call uses your saved device key (`keystore.rs`), and [`Engine::act`] saves the keys
//! you mark as verified there.
//!
//! Invariants:
//! - On the interface thread the engine only takes short locks (`sync`, `act`,
//!   `take_pictures`) and, when dropped, waits at most about a second for the call to close.
//! - Queues between threads are bounded and full queues drop media rather than block. Only
//!   the interface's commands are unbounded, and the interface sends a handful.
//! - Nothing leaves the device unencrypted: every picture and sound frame is sealed with the
//!   current MLS epoch's frame key before it reaches the network (`session.rs`).
//! - Dropping the engine leaves the call: the session task ends, links close, threads stop.

mod audio_out;
mod auto_step;
mod availability;
mod guest;
mod helper_evidence;
mod host;
mod invites;
mod loopback;
mod moderation;
mod names;
mod pinhole;
pub mod plan;
mod reach;
mod router;
mod screen_proof;
mod session;
mod sound_out;
mod sound_proof;
pub mod stats;
mod video_in;
mod video_out;
mod view;
pub(crate) mod wanted;

use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use veil_security::short_invite::InviteLink;

use crate::convert::Rgba;
use crate::data_use::DataUseCell;
use crate::keystore::KeyStore;
use crate::media::{Worker, lock};
use crate::model::{Action, Call, CallStatus};
pub use view::{SPEAKING, Shared, is_screen, screen_key, still_speaking};
use view::{Said, View};

/// One camera or screen picture, copied out for the encoder.
pub struct Captured {
    pub width: u16,
    pub height: u16,
    pub stride: usize,
    pub fps: u32,
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

/// Where the camera, screen and microphone threads hand their output to the engine.
#[derive(Clone)]
pub struct Taps {
    pub video: SyncSender<Captured>,
    pub screen: SyncSender<Captured>,
    /// One 20 ms frame of interleaved samples (`AudioSettings::voice`).
    pub audio: mpsc::Sender<Vec<f32>>,
    /// One 20 ms frame of your share's computer sound (`AudioSettings::share_sound`).
    pub share_sound: mpsc::Sender<Vec<f32>>,
}

/// Start a call, or join one from an invite.
pub enum Role {
    Host,
    Guest(Box<InviteLink>),
}

/// What the interface asks of the session task.
pub(crate) enum Command {
    Act(Action),
    Leave,
}

/// How long leaving may hold the interface while links close.
const CLOSE_WAIT: Duration = Duration::from_secs(1);
/// How long quitting may wait while a persistent call is handed over (ADR 0010 §7): up to
/// three successors' ready time, then the new host's removal commit.
const HANDOVER_WAIT: Duration = Duration::from_secs(15);

pub struct Engine {
    runtime: Option<Runtime>,
    task: Option<tokio::task::JoinHandle<()>>,
    commands: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
    keys: Arc<Mutex<KeyStore>>,
    taps: Taps,
    _workers: Vec<Worker>,
}

impl Engine {
    /// Starts the call with your device key from `keys` and the network settings `config`
    /// (`connection::Store::config`). `your_name` is sent to the other
    /// members, sealed. `playback` is the speaker and its volume, which the device pickers
    /// can change during the call. `data` is the data use choice, which the interface may
    /// change during the call (`data_use.rs`). Fails only if the async runtime cannot start.
    pub fn start(
        role: Role,
        config: plan::NetConfig,
        your_name: String,
        playback: Arc<crate::volume::Playback>,
        keys: Arc<Mutex<KeyStore>>,
        data: Arc<DataUseCell>,
        ctx: egui::Context,
    ) -> Result<Self, String> {
        let identity = lock(&keys).identity();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("veil-call")
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the call runtime: {e}"))?;
        let shared = Arc::new(Shared::new(ctx));
        shared.update(|view| view.hosting = matches!(role, Role::Host));
        shared.diagnose(|report| {
            use crate::diagnostics::{Scope, State, Step};
            report.scope = Scope {
                host: matches!(&role, Role::Host),
                local_only: config.local_only,
                relay_only: config.relay_only,
            };
            report.started = true;
            report.set(
                Step::Invitation,
                if report.scope.host {
                    State::Skipped
                } else {
                    State::Waiting
                },
            );
            report.set(
                Step::Admission,
                if report.scope.host {
                    State::Skipped
                } else {
                    State::Unchecked
                },
            );
        });
        // One picture waits at most: the encoder always takes the newest.
        let (video_tx, video_rx) = sync_channel(1);
        let (screen_tx, screen_rx) = sync_channel(1);
        let (sound_tx, sound_rx) = mpsc::channel(8);
        let (share_sound_tx, share_sound_rx) = mpsc::channel(8);
        let (encoded_tx, encoded_rx) = mpsc::channel(32);
        let (video_in_tx, video_in_rx) = sync_channel(64);
        let (audio_in_tx, audio_in_rx) = sync_channel(128);
        let (keyframe_tx, keyframe_rx) = mpsc::channel(64);
        let control = Arc::new(video_out::VideoControl::default());
        let screen_control = Arc::new(video_out::VideoControl::default());
        let camera = video_out::Stream::Camera;
        let screen = video_out::Stream::Screen;
        let workers = vec![
            video_out::spawn(
                video_rx,
                encoded_tx.clone(),
                control.clone(),
                camera,
                data.clone(),
            ),
            video_out::spawn(
                screen_rx,
                encoded_tx,
                screen_control.clone(),
                screen,
                data.clone(),
            ),
            video_in::spawn(video_in_rx, keyframe_tx, shared.clone()),
            audio_out::spawn(audio_in_rx, shared.clone(), playback),
        ];
        let pipes = session::Pipes {
            encoded: encoded_rx,
            sound: sound_rx,
            share_sound: share_sound_rx,
            keyframes: keyframe_rx,
            video_in: video_in_tx,
            audio_in: audio_in_tx,
            control,
            screen_control,
            data,
        };
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let task_shared = shared.clone();
        crate::note!(
            "call: starting as {}",
            if matches!(role, Role::Host) {
                "host"
            } else {
                "guest"
            }
        );
        let task = runtime.spawn(async move {
            let shared = task_shared.clone();
            let result = match role {
                Role::Host => {
                    let shared = task_shared;
                    host::run(identity, config, &your_name, shared, pipes, commands_rx).await
                }
                Role::Guest(invite) => {
                    let (shared, name) = (task_shared, &your_name);
                    guest::run(identity, *invite, config, name, shared, pipes, commands_rx).await
                }
            };
            let left = result.is_ok();
            let reason = match result {
                Ok(()) => String::from("You left the call"),
                Err(reason) => reason,
            };
            shared.diagnose(|report| {
                use crate::diagnostics::{End, Problem};
                report.ended = true;
                if report.end.is_none() {
                    report.end = Some(if left { End::Left } else { End::Failed });
                    if !left && report.problem.is_none() {
                        report.problem = Some(Problem::Other);
                    }
                }
            });
            shared.update(|view| view.status = CallStatus::Ended(reason));
        });
        Ok(Self {
            runtime: Some(runtime),
            task: Some(task),
            commands,
            shared,
            keys,
            taps: Taps {
                video: video_tx,
                screen: screen_tx,
                audio: sound_tx,
                share_sound: share_sound_tx,
            },
            _workers: workers,
        })
    }

    pub fn taps(&self) -> Taps {
        self.taps.clone()
    }

    /// Hands a host's decision, a chat message or a reaction to the session. Admitting or declining
    /// takes the person out of the lobby at once, so a second click cannot repeat it.
    pub fn act(&self, action: Action) {
        if let Action::Verify(id, verified) = action {
            let view = lock(&self.shared.view);
            let people = view.people.iter().map(|p| (p.id, p.device));
            let device = people
                .chain(view.lobby.iter().map(|k| (k.id, k.device)))
                .find(|(i, _)| *i == id)
                .map(|(_, d)| d);
            drop(view);
            if let Some(device) = device {
                lock(&self.keys).set_verified(device, verified);
            }
            return;
        }
        if let Action::Admit(id) | Action::Decline(id) = action {
            self.shared.update(|view| view.lobby.retain(|k| k.id != id));
        }
        // Shown at once, so the box does not flick back until the new link arrives.
        if let Action::InviteUntilEnd(until_end) = action {
            self.shared.update(|view| view.invite_until_end = until_end);
        }
        // Shown at once too. The host session applies it before the next knock it reads; if
        // the call has ended, the switch no longer matters.
        if let Action::Lobby(on) = action {
            self.shared.update(|view| view.lobby_on = on);
        }
        let _ = self.commands.send(Command::Act(action));
    }

    /// Reports each other person's tile width in physical pixels, so a call through a
    /// forwarder asks only for the layers those tiles need (`wanted.rs`).
    pub fn watch(&self, tiles: Vec<(u32, f32)>) {
        self.shared.set_tiles(tiles);
    }

    /// Who is waiting in the lobby now, by id and name.
    pub fn lobby(&self) -> Vec<(u32, String)> {
        let view = lock(&self.shared.view);
        view.lobby.iter().map(|k| (k.id, k.name.clone())).collect()
    }

    /// Copies the engine's view of the call into `call`.
    pub fn sync(&self, call: &mut Call, now: Instant) {
        let (view, notice): (View, _) = {
            let mut view = lock(&self.shared.view);
            let notice = view.handover.notice.take();
            (view.clone(), notice)
        };
        let seen = self.shared.seen();
        let keys = lock(&self.keys);
        let shown = view::apply(&view, &seen, |d| keys.is_verified(d), call, now);
        drop(keys);
        if notice.is_some() {
            call.handover.notice = notice;
        }
        for (from, said) in self.shared.take_said() {
            // Someone who left between sending and now is named by key prefix alone.
            let name = shown
                .get(&from)
                .cloned()
                .unwrap_or_else(|| format!("Guest {}", short(&from)));
            match said {
                Said::Chat { id, text } => call.receive(name, id, text, now),
                Said::React { id, emoji, on } => call.reacted(from, name, id, &emoji, on),
                Said::MuteYou => call.mute_asked = Some(name),
            }
        }
    }

    /// The newest call stats (once a second) and each person's received sound level, 0 to 1,
    /// for the debug overlay.
    pub fn stats(&self) -> (stats::Stats, HashMap<u32, f32>) {
        let levels = self
            .shared
            .seen()
            .audio
            .iter()
            .map(|(id, h)| (*id, h.level))
            .collect();
        (self.shared.stats(), levels)
    }

    /// Typed, bounded evidence only. No names, invitations, log lines or session keys.
    pub fn diagnostics(&self) -> crate::diagnostics::Snapshot {
        use crate::diagnostics::{MAX_PEERS, Peer, Snapshot};
        let report = lock(&self.shared.diagnostic).clone();
        let seen = self.shared.seen();
        let now = Instant::now();
        let fresh = |at: Instant| now.saturating_duration_since(at) < Duration::from_secs(3);
        let view = lock(&self.shared.view);
        let peers = view
            .people
            .iter()
            .take(MAX_PEERS)
            .map(|person| Peer {
                id: person.id,
                device: person.device,
                route: person.path,
                audio_recent: seen.audio.get(&person.id).is_some_and(|a| fresh(a.at)),
                // A shared screen is decoded video too; camera-off sharers are not "silent".
                video_recent: [&seen.video, &seen.screen]
                    .iter()
                    .any(|map| map.get(&person.id).is_some_and(|at| fresh(*at))),
            })
            .collect();
        drop(view);
        let stats = self.shared.stats();
        Snapshot {
            report,
            own_device: Some(lock(&self.keys).public()),
            peers,
            sampled: !stats.links.is_empty(),
            rtt_ms: stats
                .links
                .iter()
                .map(|link| link.rtt.as_millis().min(u128::from(u64::MAX)) as u64)
                .max(),
            down_kbps: stats
                .links
                .iter()
                .filter_map(|l| l.down_kbps)
                .reduce(u32::saturating_add),
            up_kbps: stats
                .links
                .iter()
                .filter_map(|l| l.up_kbps)
                .reduce(u32::saturating_add),
            ..Default::default()
        }
    }

    /// Who is sharing their screen now, by participant id.
    pub fn sharers(&self) -> Vec<u32> {
        self.shared.sharers()
    }

    /// Who is sending computer sound with their share now, by participant id.
    pub fn sounding(&self) -> Vec<u32> {
        self.shared.sounding()
    }

    /// Pictures decoded since the last call: by participant id, or [`screen_key`] of it.
    pub fn take_pictures(&self) -> Vec<(u32, Rgba)> {
        self.shared.take_pictures()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Leave);
        let started = Instant::now();
        let wait = {
            let view = lock(&self.shared.view);
            if view.hosting && view.handover.persistent {
                HANDOVER_WAIT
            } else {
                CLOSE_WAIT
            }
        };
        if let (Some(runtime), Some(task)) = (self.runtime.take(), self.task.take()) {
            runtime.block_on(async {
                if tokio::time::timeout(wait, task).await.is_err() {
                    crate::note!("call: did not close within {wait:?}; stopping it");
                }
                // Lets the endpoints send their close frames before the runtime stops.
                tokio::time::sleep(Duration::from_millis(100)).await;
            });
            runtime.shutdown_timeout(Duration::from_millis(200));
        }
        crate::note!("call: closed in {} ms", started.elapsed().as_millis());
    }
}

/// The first 8 hex digits of a key: enough to tell people apart until their name arrives.
pub fn short(key: &veil_security::PublicId) -> String {
    key.0.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Microseconds since `start`, saturating.
pub(crate) fn micros_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Ids handed to the interface for each device, stable for the whole call. You are 0.
#[derive(Default)]
pub(crate) struct Ids {
    by_device: HashMap<veil_security::PublicId, u32>,
}

impl Ids {
    pub fn id(&mut self, device: veil_security::PublicId) -> u32 {
        let next = u32::try_from(self.by_device.len()).unwrap_or(u32::MAX) + 1;
        *self.by_device.entry(device).or_insert(next)
    }

    pub fn device(&self, id: u32) -> Option<veil_security::PublicId> {
        self.by_device
            .iter()
            .find(|(_, i)| **i == id)
            .map(|(d, _)| *d)
    }
}

#[cfg(test)]
mod tests {
    use veil_security::Identity;

    use super::*;

    #[test]
    fn ids_are_stable_and_start_at_one() {
        let (a, b) = (Identity::generate().public(), Identity::generate().public());
        let mut ids = Ids::default();
        assert_eq!(ids.id(a), 1);
        assert_eq!(ids.id(b), 2);
        assert_eq!(ids.id(a), 1);
        assert_eq!(ids.device(2), Some(b));
        assert_eq!(ids.device(3), None);
        assert_eq!(short(&a).len(), 8);
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn recovery_removal_and_host_ending_retain_distinct_evidence() {
        use super::{session::diagnose_member, view::Shared};
        use crate::diagnostics::{End, Problem};
        use crate::media::lock;
        use veil_call::MemberEvent;
        let shared = Shared::new(eframe::egui::Context::default());
        diagnose_member(&shared, &MemberEvent::Reconnecting);
        assert!(lock(&shared.diagnostic).reconnecting);
        diagnose_member(&shared, &MemberEvent::Reconnected);
        assert!(!lock(&shared.diagnostic).reconnecting);
        diagnose_member(&shared, &MemberEvent::ConnectionLost);
        assert_eq!(lock(&shared.diagnostic).problem, Some(Problem::Recovery));
        diagnose_member(&shared, &MemberEvent::Reconnecting);
        diagnose_member(&shared, &MemberEvent::Removed);
        assert!(
            !lock(&shared.diagnostic).reconnecting,
            "removal stops recovery evidence"
        );
        assert_eq!(lock(&shared.diagnostic).end, Some(End::Removed));
        assert_eq!(lock(&shared.diagnostic).problem, Some(Problem::Removed));
        let shared = Shared::new(eframe::egui::Context::default());
        diagnose_member(&shared, &MemberEvent::Closed);
        assert_eq!(lock(&shared.diagnostic).end, Some(End::HostEnded));
    }

    #[test]
    fn a_shared_screen_counts_as_that_sharers_decoded_video() {
        use super::view::{Shared, screen_key};
        use crate::convert::Rgba;
        let shared = Shared::new(eframe::egui::Context::default());
        let rgba = || Rgba {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        };
        shared.put_picture(screen_key(7), rgba(), (2, 2));
        let seen = shared.seen();
        assert!(seen.video.is_empty(), "a screen is not a camera tile");
        assert!(
            seen.screen.contains_key(&7),
            "keyed by the sharer, not the screen key"
        );
        shared.put_picture(9, rgba(), (2, 2));
        let seen = shared.seen();
        assert!(seen.video.contains_key(&9) && !seen.screen.contains_key(&9));
    }
}
