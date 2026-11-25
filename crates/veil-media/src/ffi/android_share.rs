//! Android screen share bridge (SHARE-02). `ScreenShare.java` owns the MediaProjection, its
//! virtual display, image reader and playback capture. Rust asks by generation and receives
//! copies, checked against a size cap before they are converted (`projection.rs`).
//!
//! Invariants:
//! - Every call goes through [`super::android::activity`], so local JNI references stay in
//!   their attachment scope and no worker keeps a borrowed reference.
//! - Lengths are checked on the Java array before its bytes are copied into Rust.

use jni::objects::{JByteArray, JValue};
use jni::{jni_sig, jni_str};

use super::android::activity;
use crate::MediaError;
use crate::projection::{MAX_FRAME_BYTES, MAX_SOUND_BYTES, Status};

fn generation_arg(generation: u32) -> JValue<'static> {
    JValue::Int(i32::try_from(generation).unwrap_or(i32::MAX))
}

/// Opens Android's consent prompt for a new share. Returns its generation.
pub fn request() -> Result<u32, MediaError> {
    let generation = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareRequest"),
            jni_sig!(() -> int),
            &[],
        )?
        .i()
    })?;
    u32::try_from(generation).map_err(|_| MediaError::Unavailable("screen sharing"))
}

pub fn status(generation: u32) -> Status {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareStatus"),
            jni_sig!((int) -> int),
            &[generation_arg(generation)],
        )?
        .i()
    })
    .map_or(Status::Ended, Status::from_code)
}

/// The newest picture since the last call, copied: `None` if there is none yet.
pub fn frame(generation: u32) -> Result<Option<Vec<u8>>, MediaError> {
    activity(|env, object| {
        let result = env
            .call_method(
                object,
                jni_str!("veilShareFrame"),
                jni_sig!((int) -> byte[]),
                &[generation_arg(generation)],
            )?
            .l()?;
        if result.is_null() {
            return Ok(None);
        }
        let result = JByteArray::cast_local(env, result)?;
        if result.len(env)? > MAX_FRAME_BYTES {
            return Ok(Some(Vec::new()));
        }
        env.convert_byte_array(result).map(Some)
    })
}

/// Stops the projection for `generation`, if it is still the current one.
pub fn stop(generation: u32) {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareStop"),
            jni_sig!((int) -> void),
            &[generation_arg(generation)],
        )?;
        Ok(())
    });
}

/// Whether Veil may record sound, which playback capture needs.
pub fn sound_permitted() -> bool {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareSoundPermitted"),
            jni_sig!(() -> boolean),
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Starts capturing what other apps play during the share `generation`.
pub fn sound_start(generation: u32) -> bool {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareSoundStart"),
            jni_sig!((int) -> boolean),
            &[generation_arg(generation)],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Sound that has arrived since the last call, as 16-bit stereo bytes; empty if none.
/// An error means sound capture has ended.
pub fn sound_read(generation: u32) -> Result<Vec<u8>, MediaError> {
    let read = activity(|env, object| {
        let result = env
            .call_method(
                object,
                jni_str!("veilShareSoundRead"),
                jni_sig!((int) -> byte[]),
                &[generation_arg(generation)],
            )?
            .l()?;
        if result.is_null() {
            return Ok(None);
        }
        let result = JByteArray::cast_local(env, result)?;
        if result.len(env)? > MAX_SOUND_BYTES {
            return Ok(Some(Vec::new()));
        }
        env.convert_byte_array(result).map(Some)
    })?;
    read.ok_or(MediaError::Capture("phone sound stopped"))
}

pub fn sound_stop(generation: u32) {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilShareSoundStop"),
            jni_sig!((int) -> void),
            &[generation_arg(generation)],
        )?;
        Ok(())
    });
}
