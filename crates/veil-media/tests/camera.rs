//! Windows camera capture against the real Media Foundation source reader.
//!
//! `cameras_are_listed` runs always and does not open a camera. `camera_gives_nv12_pictures`
//! turns the camera on for about a second, so it is ignored by default:
//! `cargo test -j 8 -p veil-media --test camera -- --ignored --nocapture`.
//! Nothing captured is stored or sent.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]

use veil_media::{CameraMode, MfCamera, cameras};

#[test]
fn cameras_are_listed() {
    let list = cameras().unwrap();
    for camera in &list {
        assert!(!camera.name.is_empty());
        assert!(!camera.id.is_empty(), "{} has no id", camera.name);
        println!("camera: {}", camera.name);
    }
}

#[test]
#[ignore = "turns the camera on"]
fn camera_gives_nv12_pictures() {
    if cameras().unwrap().is_empty() {
        println!("no camera on this machine, skipped");
        return;
    }
    let want = CameraMode {
        width: 1_920,
        height: 1_080,
        fps: 30,
    };
    let mut camera = MfCamera::open(None, want).unwrap();
    let mode = camera.mode();
    println!("{}: {mode:?}", camera.name());
    let mut last = None;
    let mut pictures = 0;
    for _ in 0..200 {
        let Some(picture) = camera.read().unwrap() else {
            continue;
        };
        assert_eq!((picture.width, picture.height), (mode.width, mode.height));
        assert_eq!(picture.stride, usize::from(mode.width));
        let size = usize::from(mode.width) * usize::from(mode.height) * 3 / 2;
        assert_eq!(picture.data.len(), size);
        if let Some(last) = last {
            assert!(picture.timestamp_us > last, "timestamps must increase");
        }
        last = Some(picture.timestamp_us);
        pictures += 1;
        if pictures == 30 {
            break;
        }
    }
    assert_eq!(pictures, 30, "camera gave too few pictures");
}
