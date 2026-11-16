//! WASAPI on this machine: playback paces the caller to the device, and the microphone gives
//! whole frames. Playback writes silence. Skips, with a note, when there is no audio device.
//! The microphone test is ignored by default because it turns the microphone on:
//! `cargo test -j 8 -p veil-media --test wasapi -- --ignored --nocapture`.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use veil_media::{AudioSettings, MediaError, WasapiCapture, WasapiPlayback};

#[test]
fn playback_keeps_pace_with_the_device() {
    let settings = AudioSettings::voice();
    let mut speakers = match WasapiPlayback::open(&settings) {
        Ok(speakers) => speakers,
        Err(MediaError::Unavailable(what)) => {
            eprintln!("skipped: no {what} on this machine");
            return;
        }
        Err(e) => panic!("open: {e}"),
    };
    let silence = vec![0.0f32; settings.frame_samples()];
    assert!(speakers.write(&silence[1..]).is_err());
    let start = Instant::now();
    for _ in 0..25 {
        speakers.write(&silence).unwrap();
        assert!(speakers.queued_ms().unwrap() <= 40);
    }
    // 25 frames of 20 ms, at most two still queued, so about 460 ms. The engine also reads up
    // to a few periods ahead when it starts, so allow 60 ms less. Written without pacing, the
    // loop would take under a millisecond.
    let elapsed = start.elapsed();
    eprintln!("25 frames written in {elapsed:?}");
    assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    assert!(elapsed <= Duration::from_millis(700), "{elapsed:?}");
}

#[test]
#[ignore = "turns the microphone on"]
fn microphone_gives_whole_frames_in_real_time() {
    let settings = AudioSettings::voice();
    let mut microphone = WasapiCapture::open(&settings).unwrap();
    let start = Instant::now();
    let mut peak = 0.0f32;
    for _ in 0..50 {
        let frame = microphone.read().unwrap();
        assert_eq!(frame.len(), settings.frame_samples());
        peak = frame.iter().fold(peak, |p, s| p.max(s.abs()));
    }
    let elapsed = start.elapsed();
    eprintln!("50 frames read in {elapsed:?}, peak level {peak:.3}");
    // One second of audio, less what was already buffered when reading began.
    assert!(elapsed >= Duration::from_millis(800), "{elapsed:?}");
    assert!(elapsed <= Duration::from_millis(1_300), "{elapsed:?}");
}
