//! Two engines on this machine, host and guest, through the whole pipeline.
//!
//! Purpose: proves the glue without a window or devices. The host starts, the guest joins from
//! its invite and waits in the lobby, the host admits, and both feed synthetic pictures and
//! silence into their taps. Each must then see the other's video decoded and hear packets,
//! show the name the other chose, and receive the other's chat message.
//!
//! Ignored by default: it opens UDP ports (Windows may ask about the firewall), the GPU or
//! software H.264 encoder, and the default speaker (it plays silence). Run it with
//! `cargo test -j 8 -p veil-app loopback -- --ignored --nocapture`.

#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use veil_security::short_invite::InviteLink;

use super::plan::NetConfig;
use super::{Captured, Engine, Role, Taps, screen_key};
use crate::model::{Action, Call, CallStatus};

const WIDTH: u16 = 1_280;
const HEIGHT: u16 = 720;

fn wait_for(what: &str, engine: &Engine, call: &mut Call, done: impl Fn(&Call) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        engine.sync(call, Instant::now());
        if done(call) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}: {:?}", call.status);
}

/// One synthetic picture: a grey frame whose brightness moves, so the encoder has work.
fn picture(n: u64) -> Captured {
    let (w, h) = (usize::from(WIDTH), usize::from(HEIGHT));
    let mut data = vec![128u8; w * h * 3 / 2];
    let luma = 40 + (n % 160) as u8;
    data[..w * h].fill(luma);
    Captured {
        width: WIDTH,
        height: HEIGHT,
        stride: w,
        fps: 30,
        timestamp_us: n * 33_333,
        data,
    }
}

fn feed(taps: &[Taps], n: u64) {
    for tap in taps {
        let _ = tap.video.try_send(picture(n));
        // Two 20 ms frames of silence per 33 ms picture keeps sound ahead of real time.
        for _ in 0..2 {
            let _ = tap.audio.try_send(vec![0.0; 960]);
        }
    }
}

#[test]
#[ignore = "opens ports, the H.264 encoder and the speaker"]
fn loopback_host_admits_a_guest_and_both_see_and_hear_each_other() {
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Hana Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Hana Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let first_invite = host_call.invite.clone();
    let invite = InviteLink::parse(&first_invite, now).unwrap();
    // A machine with a public IPv6 address keeps the full link, because this host's own
    // rendezvous cannot reach as far (`invites::can_shorten`). Either link must work here.
    // shortcut: short links join here only without public IPv6, veil-call's short_link test
    // covers them everywhere; upgrade if this test gains a public rendezvous.
    assert_eq!(
        matches!(invite, InviteLink::Short(_)),
        first_invite != host_call.invite_full,
        "the link shown is the short one exactly when it differs from the full one"
    );
    println!("invite: {}", host_call.invite);

    let guest =
        Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Gil Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    let mut guest_call = Call::live("Call".into(), "Gil Guest", false, Instant::now());
    wait_for("the lobby", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Lobby
    });
    wait_for("a knock", &host, &mut host_call, |c| c.lobby.len() == 1);
    let knocker = host_call.lobby[0].id;
    host.act(Action::Admit(knocker));
    wait_for("admission", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Live && c.participants.len() == 2
    });
    wait_for("the guest in the roster", &host, &mut host_call, |c| {
        c.participants.len() == 2 && c.lobby.is_empty()
    });
    assert_ne!(host_call.invite, first_invite, "a used invite is replaced");
    assert_eq!(
        host_call.participants[1].safety, guest_call.participants[1].safety,
        "both sides show the same safety number"
    );
    assert_ne!(
        host_call.participants[1].side, guest_call.participants[1].side,
        "each side has its own share code"
    );
    assert!(guest_call.participants[1].is_host);
    assert!(host_call.epoch >= 1 && host_call.epoch == guest_call.epoch);

    let taps = [host.taps(), guest.taps()];
    let start = Instant::now();
    let mut n = 0u64;
    let both = |host: &Call, guest: &Call| {
        let seen = |c: &Call| c.participants.get(1).is_some_and(|p| p.camera && !p.muted);
        seen(host) && seen(guest)
    };
    while start.elapsed() < Duration::from_secs(20) {
        feed(&taps, n);
        n += 1;
        std::thread::sleep(Duration::from_millis(33));
        host.sync(&mut host_call, Instant::now());
        guest.sync(&mut guest_call, Instant::now());
        if both(&host_call, &guest_call) {
            break;
        }
    }
    let pictures = (host.take_pictures().len(), guest.take_pictures().len());
    println!(
        "after {} pictures: host sees {:?}, guest sees {:?}, paths {:?} / {:?}",
        n,
        host_call.participants[1],
        guest_call.participants[1],
        host_call.participants[1].path,
        guest_call.participants[1].path,
    );
    assert!(
        both(&host_call, &guest_call),
        "video and sound both ways (pictures waiting: {pictures:?})"
    );

    // Names: each side shows the name the other chose, not "Host" or "Guest" and a key.
    wait_for("the host's name", &guest, &mut guest_call, |c| {
        c.participants.get(1).is_some_and(|p| p.name == "Hana Host")
    });
    wait_for("the guest's name", &host, &mut host_call, |c| {
        c.participants.get(1).is_some_and(|p| p.name == "Gil Guest")
    });

    // Chat, both ways, through the interface's own path (`Call::send`, then the outbox).
    let send = |engine: &Engine, call: &mut Call, text: &str| {
        call.send(text, Instant::now());
        for action in call.outbox.drain(..) {
            engine.act(action);
        }
    };
    let got = |from: &'static str, text: &'static str| {
        move |c: &Call| {
            c.chat
                .iter()
                .any(|m| !m.mine && m.from == from && m.text == text)
        }
    };
    send(&host, &mut host_call, "Hello from the host");
    wait_for(
        "the host's chat",
        &guest,
        &mut guest_call,
        got("Hana Host", "Hello from the host"),
    );
    send(
        &guest,
        &mut guest_call,
        "Hello back
from the guest",
    );
    let back = got(
        "Gil Guest",
        "Hello back
from the guest",
    );
    wait_for("the guest's chat", &host, &mut host_call, back);
    assert_eq!((host_call.unread, guest_call.unread), (1, 1));
    println!("names and chat crossed both ways");

    drop(guest);
    wait_for("the guest to leave", &host, &mut host_call, |c| {
        c.participants.len() == 1
    });
}

#[test]
#[ignore = "turns the camera and microphone on, and opens ports"]
fn loopback_leaving_a_call_with_devices_on_is_quick() {
    let ctx = egui::Context::default();
    let mut media = crate::media::LocalMedia::new(ctx.clone());
    media.set_camera(true);
    media.set_mic(true);
    let engine = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    media.set_taps(Some(engine.taps()));
    std::thread::sleep(Duration::from_secs(3));
    let start = Instant::now();
    drop(engine);
    println!("engine dropped in {:?}", start.elapsed());
    let start = Instant::now();
    drop(media);
    println!("devices closed in {:?}", start.elapsed());
}

/// Joining a host that has gone must never hold the interface thread: each `sync` stays
/// short while the guest tries to connect, and leaving returns within about a second.
#[test]
#[ignore = "opens ports and the speaker"]
fn loopback_joining_an_unreachable_host_never_holds_the_interface() {
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let invite = InviteLink::parse(&host_call.invite_full, now).unwrap();
    drop(host);

    // Leaving while still reaching the host (the hang Windows logged on 16/11/2026).
    let early = Engine::start(Role::Guest(Box::new(invite.clone())), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let t = Instant::now();
    drop(early);
    println!("left while connecting in {:?}", t.elapsed());
    assert!(t.elapsed() < Duration::from_secs(2), "leaving while connecting held the interface");

    let started = Instant::now();
    let guest = Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    println!("guest engine started in {:?}", started.elapsed());
    let mut call = Call::live("Call".into(), "Guest", false, Instant::now());
    let mut slowest = Duration::ZERO;
    let watch = Instant::now();
    while watch.elapsed() < Duration::from_secs(40) {
        let t = Instant::now();
        guest.sync(&mut call, Instant::now());
        slowest = slowest.max(t.elapsed());
        if matches!(call.status, CallStatus::Ended(_)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("after {:?}: {:?}, slowest sync {slowest:?}", watch.elapsed(), call.status);
    let t = Instant::now();
    drop(guest);
    println!("guest left in {:?}", t.elapsed());
    assert!(slowest < Duration::from_millis(100), "sync held the interface for {slowest:?}");
    assert!(t.elapsed() < Duration::from_secs(2), "leaving held the interface");
}

/// The host leaving ends the call for the guest within a second, with the reason shown.
#[test]
#[ignore = "opens ports and the speaker"]
fn loopback_guest_sees_the_host_end_the_call() {
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let invite = InviteLink::parse(&host_call.invite_full, now).unwrap();
    let guest = Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    let mut guest_call = Call::live("Call".into(), "Guest", false, Instant::now());
    wait_for("a knock", &host, &mut host_call, |c| c.lobby.len() == 1);
    host.act(Action::Admit(host_call.lobby[0].id));
    wait_for("admission", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Live && c.participants.len() == 2
    });

    drop(host);
    let t = Instant::now();
    wait_for("the call to end", &guest, &mut guest_call, |c| {
        matches!(c.status, CallStatus::Ended(_))
    });
    println!("guest saw {:?} after {:?}", guest_call.status, t.elapsed());
    assert!(t.elapsed() < Duration::from_secs(1), "the guest heard late");
    assert_eq!(guest_call.status, CallStatus::Ended("The host ended the call.".into()));
}

/// A raised hand reaches the others, lowering it clears it for them, and someone who joins
/// later sees a hand that is already up.
#[test]
#[ignore = "opens ports and the speaker"]
fn loopback_a_raised_hand_reaches_the_others() {
    let hand = |engine: &Engine, call: &mut Call, raised: bool| {
        call.raise_hand(raised);
        for action in call.outbox.drain(..) {
            engine.act(action);
        }
    };
    let theirs = |raised: bool| move |c: &Call| c.participants.get(1).is_some_and(|p| p.hand_raised == raised);
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    // Raised while alone: the guest joins late and must still see it.
    hand(&host, &mut host_call, true);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let invite = InviteLink::parse(&host_call.invite_full, now).unwrap();
    let guest = Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    let mut guest_call = Call::live("Call".into(), "Guest", false, Instant::now());
    wait_for("a knock", &host, &mut host_call, |c| c.lobby.len() == 1);
    host.act(Action::Admit(host_call.lobby[0].id));
    wait_for("admission", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Live && c.participants.len() == 2
    });
    wait_for("the host's hand, already up", &guest, &mut guest_call, theirs(true));

    wait_for("the guest in the roster", &host, &mut host_call, |c| c.participants.len() == 2);
    assert!(!host_call.participants[1].hand_raised, "the guest's hand starts down");
    hand(&guest, &mut guest_call, true);
    wait_for("the guest's hand", &host, &mut host_call, theirs(true));
    hand(&guest, &mut guest_call, false);
    wait_for("the guest's hand lowered", &host, &mut host_call, theirs(false));
    hand(&host, &mut host_call, false);
    wait_for("the host's hand lowered", &guest, &mut guest_call, theirs(false));
}

/// A shared screen arrives as its own stream, beside the sender's camera.
#[test]
#[ignore = "opens ports, the H.264 encoder and the speaker"]
fn loopback_a_shared_screen_arrives_beside_the_camera() {
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let invite = InviteLink::parse(&host_call.invite_full, now).unwrap();
    let guest = Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    let mut guest_call = Call::live("Call".into(), "Guest", false, Instant::now());
    wait_for("a knock", &host, &mut host_call, |c| c.lobby.len() == 1);
    host.act(Action::Admit(host_call.lobby[0].id));
    wait_for("admission", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Live && c.participants.len() == 2
    });
    let sender = guest_call.participants[1].id;
    let taps = host.taps();
    let start = Instant::now();
    let mut keys = Vec::new();
    let mut n = 0;
    while start.elapsed() < Duration::from_secs(20) {
        let _ = taps.video.try_send(picture(n));
        let _ = taps.screen.try_send(picture(n));
        n += 1;
        std::thread::sleep(Duration::from_millis(33));
        keys.extend(guest.take_pictures().into_iter().map(|(key, _)| key));
        if keys.contains(&sender) && keys.contains(&screen_key(sender)) {
            break;
        }
    }
    assert!(keys.contains(&sender), "the camera arrives");
    assert!(keys.contains(&screen_key(sender)), "the screen arrives");
    assert_eq!(guest.sharers(), [sender]);
    assert!(host.sharers().is_empty(), "only the guest received a share");

    // Stopping is silence: those watching drop the share after a few seconds.
    let stopped = Instant::now();
    while !guest.sharers().is_empty() && stopped.elapsed() < Duration::from_secs(8) {
        let _ = taps.video.try_send(picture(n));
        n += 1;
        std::thread::sleep(Duration::from_millis(33));
    }
    assert!(guest.sharers().is_empty(), "the share ends for the guest");
}

/// A shared area chosen, then changed, mid-share: each new size reopens the sender's encoder,
/// and those watching must follow it rather than keep the old picture.
#[test]
#[ignore = "opens ports, the H.264 encoder and the speaker"]
fn loopback_a_shared_area_that_changes_size_keeps_moving() {
    let ctx = egui::Context::default();
    let host = Engine::start(Role::Host, NetConfig::default(), "Host".into(), Default::default(), fresh_keys(), Default::default(), ctx.clone()).unwrap();
    let mut host_call = Call::live("Call".into(), "Host", true, Instant::now());
    wait_for("an invite", &host, &mut host_call, |c| !c.invite.is_empty());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let invite = InviteLink::parse(&host_call.invite_full, now).unwrap();
    let guest = Engine::start(Role::Guest(Box::new(invite)), NetConfig::default(), "Guest".into(), Default::default(), fresh_keys(), Default::default(), ctx).unwrap();
    let mut guest_call = Call::live("Call".into(), "Guest", false, Instant::now());
    wait_for("a knock", &host, &mut host_call, |c| c.lobby.len() == 1);
    host.act(Action::Admit(host_call.lobby[0].id));
    wait_for("admission", &guest, &mut guest_call, |c| {
        c.status == CallStatus::Live && c.participants.len() == 2
    });
    let sender = guest_call.participants[1].id;
    let taps = host.taps();
    let mut n = 0u64;
    // A whole screen, then an area, a different area, and the whole screen again.
    for (w, h) in [(1_280u16, 720u16), (960, 540), (802, 600), (1_280, 720)] {
        let start = Instant::now();
        let mut moving = 0;
        let mut last = Vec::new();
        while moving < 10 && start.elapsed() < Duration::from_secs(10) {
            let (wu, hu) = (usize::from(w), usize::from(h));
            let mut data = vec![128u8; wu * hu * 3 / 2];
            data[..wu * hu].fill(40 + (n % 160) as u8);
            let screen = Captured { width: w, height: h, stride: wu, fps: 30, timestamp_us: n * 33_333, data };
            let _ = taps.screen.try_send(screen);
            n += 1;
            std::thread::sleep(Duration::from_millis(33));
            for (key, rgba) in guest.take_pictures() {
                let fresh = rgba.pixels != last;
                if key == screen_key(sender) && (rgba.width, rgba.height) == (wu, hu) && fresh {
                    moving += 1;
                    last = rgba.pixels;
                }
            }
        }
        assert!(moving >= 10, "{w}x{h}: only {moving} new pictures of that size arrived");
    }
}

/// A key that lasts this test only: nothing is saved.
fn fresh_keys() -> std::sync::Arc<std::sync::Mutex<crate::keystore::KeyStore>> {
    std::sync::Arc::new(std::sync::Mutex::new(crate::keystore::KeyStore::open(None)))
}
