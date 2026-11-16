//! Known-answer tests for Veil's wire formats.
//!
//! Purpose: pin the exact bytes of frame encryption, safety numbers, invite links and lobby
//! passes. These must never change silently, for example during a crypto crate upgrade: two
//! Veil builds on different library versions must still talk to each other. The expected
//! values were captured from the RustCrypto 0.10 generation (chacha20poly1305 0.10, hkdf
//! 0.12, sha2 0.10, ed25519-dalek 2.2) before the upgrade to the next generation.
#![allow(clippy::unwrap_used)]

use veil_security::frame::{FrameReceiver, FrameSender};
use veil_security::invite::{Invite, VERSION, lobby_pass};
use veil_security::safety::safety_number;
use veil_security::{Identity, Layer};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const EPOCH_SECRET: [u8; 32] = [0x42; 32];

#[test]
fn frame_encryption_bytes_are_fixed() {
    let mut tx = FrameSender::new(&EPOCH_SECRET, 3, 5).unwrap();
    let first = tx.encrypt(b"veil frame one").unwrap();
    let second = tx.encrypt_layer(Layer::BASE, b"veil frame two").unwrap();
    assert_eq!(hex(&first), "000000000003000500000000000000000000fd3122f4cb2886c25195032e5e478bccc149cef28bb041c13f335c34f0bb");
    assert_eq!(hex(&second), "0000000000030005000000000000000100009ea37c20422e7b0552b9a1d556f9d50d80dbf870f28669bc0822a504976c");
    let mut rx = FrameReceiver::new(&EPOCH_SECRET, 3).unwrap();
    assert_eq!(rx.decrypt(&first).unwrap(), (5, b"veil frame one".to_vec()));
}

#[test]
fn identity_public_key_is_fixed() {
    let id = Identity::from_secret(&[1; 32]);
    assert_eq!(hex(&id.public().0), "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c");
    assert_eq!(hex(&id.sign(b"veil")), "990f3fa700a2caae27a96b30631fca1bed61cef7f31f9ea0d11684e5e24620268b38a95f94cea52690b4536b243b9e18dd5c9faccad6548b085aef5ed175510c");
}

#[test]
fn safety_number_is_fixed() {
    let a = Identity::from_secret(&[1; 32]).public();
    let b = Identity::from_secret(&[2; 32]).public();
    assert_eq!(safety_number(&a, &b), "64289 89746 90840 82925 33687 44699 07413 00228 18081 69627 31945 75269");
}

#[test]
fn invite_link_is_fixed() {
    let host = Identity::from_secret(&[3; 32]);
    let invite = Invite {
        version: VERSION,
        room_id: [4; 16],
        host: host.public(),
        addrs: vec!["203.0.113.7:4433".into()],
        relay: Some("relay.example:4433".into()),
        expires_unix: 1_800_000_000,
        join_token: [5; 16],
    };
    let link = invite.to_link(&host).unwrap();
    assert_eq!(link, "veil:AQQEBAQEBAQEBAQEBAQEBATtSSjGKNHCxurpAziQWZVhKVknOlxj-TY2wUYUrIc30QEQMjAzLjAuMTEzLjc6NDQzMwEScmVsYXkuZXhhbXBsZTo0NDMzgKSn2gYFBQUFBQUFBQUFBQUFBQUFXDmmG12JKqATgmdSfk-RhgDPV-s3uNv7oZe647eZ_4BB9F7Tg7bi7lUMXfNpYscXeglfTYCA60IydscQOqH-Cg");
    assert_eq!(Invite::from_link(&link, 1_700_000_000).unwrap(), invite);
}

#[test]
fn lobby_pass_is_fixed() {
    assert_eq!(hex(&lobby_pass(&[6; 16])), "ee5c128413a13c7b91f5b13ea4e3db0f2c57745e381282e279ce9e0a9c8f00b3");
}
