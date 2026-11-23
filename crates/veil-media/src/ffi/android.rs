//! Android Camera2 and Keystore bridge. Java owns each native camera resource; Rust only
//! receives bounded, copied NV12 frames. Local JNI references stay in their attachment scope.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jni::objects::{JByteArray, JObject, JString, JValue};
use jni::refs::Global;
use jni::{Env, JavaVM, jni_sig, jni_str};
use zeroize::Zeroize;

use crate::capture::{CameraMode, NativeMode, PixelFormat};
use crate::{MediaError, RawPicture};

struct Bridge {
    vm: JavaVM,
    activity: Global<JObject<'static>>,
}
static APP: Mutex<Option<Arc<Bridge>>> = Mutex::new(None);

/// The Activity, rather than ndk-context's Application, owns Veil's platform methods.
pub fn init(app: android_activity::AndroidApp) -> Result<(), MediaError> {
    // SAFETY: AndroidApp supplies the live process VM. Create our own global reference
    // before entering the event loop, so media callbacks never lock AndroidApp's lifecycle.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let raw = app.activity_as_ptr();
    let global = vm
        .attach_current_thread(|env| -> jni::errors::Result<_> {
            // SAFETY: AndroidApp owns this valid global reference until this function returns.
            let object = unsafe { JObject::from_raw(env, raw.cast()) };
            env.new_global_ref(object)
        })
        .map_err(|_| MediaError::Unavailable("Android Activity"))?;
    *APP.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(Bridge {
        vm,
        activity: global,
    }));
    Ok(())
}

fn activity<T>(
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T, MediaError> {
    let app = APP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(MediaError::Unavailable("Android Activity"))?;
    app.vm
        .attach_current_thread(|env| f(env, app.activity.as_ref()))
        .map_err(|_| MediaError::Unavailable("Android platform service; check permissions"))
}

pub fn data_dir() -> Result<PathBuf, MediaError> {
    activity(|env, object| {
        let result = env
            .call_method(
                object,
                jni_str!("veilDataDir"),
                jni_sig!(() -> java.lang.String),
                &[],
            )?
            .l()?;
        Ok(PathBuf::from(JString::cast_local(env, result)?.to_string()))
    })
}

pub fn protect(bytes: &[u8], label: &[u8], seal: bool) -> Result<Vec<u8>, MediaError> {
    activity(|env, object| {
        let bytes = env.byte_array_from_slice(bytes)?;
        let label = env.byte_array_from_slice(label)?;
        let result = env
            .call_method(
                object,
                jni_str!("veilProtect"),
                jni_sig!((byte[], byte[], boolean) -> byte[]),
                &[
                    JValue::Object(bytes.as_ref()),
                    JValue::Object(label.as_ref()),
                    JValue::Bool(seal),
                ],
            )?
            .l()?;
        let result = JByteArray::cast_local(env, result)?;
        let mut output = env.convert_byte_array(&result)?;
        if let Err(problem) = result.set_region(env, 0, &vec![0; output.len()]) {
            output.zeroize();
            return Err(problem);
        }
        Ok(output)
    })
}

pub fn request_permissions() {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilRequestPermissions"),
            jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    });
}

pub fn resumed() -> bool {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilResumed"),
            jni_sig!(() -> boolean),
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

pub fn background() {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilBackground"),
            jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    });
}

pub fn insets() -> (u32, u32) {
    let packed = activity(|env, object| {
        env.call_method(object, jni_str!("veilInsets"), jni_sig!(() -> long), &[])?
            .j()
    })
    .unwrap_or(0) as u64;
    ((packed >> 32) as u32, packed as u32)
}

pub fn font_scale() -> f32 {
    let scale = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilFontScale"),
            jni_sig!(() -> float),
            &[],
        )?
        .f()
    })
    .unwrap_or(1.0);
    if scale.is_finite() && (0.5..=3.0).contains(&scale) {
        scale
    } else {
        1.0
    }
}

pub fn side_insets() -> (u32, u32) {
    let packed = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilSideInsets"),
            jni_sig!(() -> long),
            &[],
        )?
        .j()
    })
    .unwrap_or(0) as u64;
    ((packed >> 32) as u32, packed as u32)
}

pub fn take_invite() -> Option<String> {
    activity(|env, object| {
        let result = env
            .call_method(
                object,
                jni_str!("veilTakeInvite"),
                jni_sig!(() -> java.lang.String),
                &[],
            )?
            .l()?;
        Ok(JString::cast_local(env, result)?.to_string())
    })
    .ok()
    .filter(|value| !value.is_empty())
}

/// The Android clipboard is used explicitly by the Paste and Copy controls.
pub fn paste_text() -> Result<String, MediaError> {
    activity(|env, object| {
        let text = env.new_string("")?;
        let result = env
            .call_method(
                object,
                jni_str!("veilClipboard"),
                jni_sig!((java.lang.String, boolean) -> java.lang.String),
                &[JValue::Object(text.as_ref()), JValue::Bool(false)],
            )?
            .l()?;
        Ok(JString::cast_local(env, result)?.to_string())
    })
}

pub fn permissions_ready() -> bool {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilPermissionsReady"),
            jni_sig!(() -> boolean),
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Camera and microphone permissions are independent: declining video must not mute audio.
pub fn permission_state() -> (bool, bool) {
    let bits = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilPermissionState"),
            jni_sig!(() -> int),
            &[],
        )?
        .i()
    })
    .unwrap_or(0);
    (bits & 1 != 0, bits & 2 != 0)
}

pub fn back_requested() -> bool {
    activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilBackRequested"),
            jni_sig!(() -> boolean),
            &[],
        )?
        .z()
    })
    .unwrap_or(false)
}

pub fn call_active(active: bool) {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilCallActive"),
            jni_sig!((boolean) -> void),
            &[JValue::Bool(active)],
        )?;
        Ok(())
    });
}

pub fn show_notification(_: &str, heading: &str, body: &str) -> Result<(), String> {
    activity(|env, object| {
        let heading = env.new_string(heading)?;
        let body = env.new_string(body)?;
        env.call_method(
            object,
            jni_str!("veilNotify"),
            jni_sig!((java.lang.String, java.lang.String) -> void),
            &[
                JValue::Object(heading.as_ref()),
                JValue::Object(body.as_ref()),
            ],
        )?;
        Ok(())
    })
    .map_err(|error| error.to_string())
}

pub fn clear_notifications() {
    let _ = activity(|env, object| {
        env.call_method(
            object,
            jni_str!("veilClearNotifications"),
            jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    });
}

pub fn copy_text(value: &str) {
    let _ = activity(|env, object| {
        let value = env.new_string(value)?;
        env.call_method(
            object,
            jni_str!("veilClipboard"),
            jni_sig!((java.lang.String, boolean) -> java.lang.String),
            &[JValue::Object(value.as_ref()), JValue::Bool(true)],
        )?;
        Ok(())
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub name: String,
    pub id: String,
}

pub fn cameras() -> Result<Vec<CameraInfo>, MediaError> {
    let list = activity(|env, object| {
        let result = env
            .call_method(
                object,
                jni_str!("veilCameras"),
                jni_sig!(() -> java.lang.String),
                &[],
            )?
            .l()?;
        Ok(JString::cast_local(env, result)?.to_string())
    })?;
    Ok(list
        .lines()
        .filter_map(|line| {
            let (id, name) = line.split_once('\t')?;
            Some(CameraInfo {
                name: name.into(),
                id: id.into(),
            })
        })
        .collect())
}

pub struct Camera {
    mode: NativeMode,
    frame: Vec<u8>,
    width: u16,
    height: u16,
    clock: Instant,
    last: Instant,
}

impl Camera {
    pub fn open(id: Option<&str>, _: CameraMode) -> Result<Self, MediaError> {
        let opened = activity(|env, object| {
            let id = env.new_string(id.unwrap_or_default())?;
            env.call_method(
                object,
                jni_str!("veilOpenCamera"),
                jni_sig!((java.lang.String) -> boolean),
                &[JValue::Object(id.as_ref())],
            )?
            .z()
        })?;
        if !opened {
            return Err(MediaError::Unavailable("camera permission or camera"));
        }
        Ok(Self {
            mode: NativeMode {
                width: 480,
                height: 640,
                fps_num: 30,
                fps_den: 1,
                format: PixelFormat::Nv12,
            },
            frame: Vec::new(),
            width: 480,
            height: 640,
            clock: Instant::now(),
            last: Instant::now(),
        })
    }

    pub fn mode(&self) -> NativeMode {
        self.mode
    }

    pub fn read(&mut self) -> Result<Option<RawPicture<'_>>, MediaError> {
        let frame = activity(|env, object| {
            let result = env
                .call_method(
                    object,
                    jni_str!("veilCameraFrame"),
                    jni_sig!(() -> byte[]),
                    &[],
                )?
                .l()?;
            if result.is_null() {
                return Ok(None);
            }
            let result = JByteArray::cast_local(env, result)?;
            let length = result.len(env)?;
            if length > 1920 * 1080 * 3 / 2 + 4 {
                return Ok(None);
            }
            env.convert_byte_array(result).map(Some)
        })?;
        let Some(frame) = frame else {
            if self.last.elapsed() > Duration::from_secs(5) {
                return Err(MediaError::Capture("camera stopped delivering frames"));
            }
            std::thread::sleep(Duration::from_millis(5));
            return Ok(None);
        };
        let Some(header) = frame.get(..4) else {
            return Err(MediaError::Malformed("Android camera frame"));
        };
        self.width = u16::from_le_bytes([header[0], header[1]]);
        self.height = u16::from_le_bytes([header[2], header[3]]);
        self.frame = frame;
        self.last = Instant::now();
        RawPicture::new(
            self.width,
            self.height,
            usize::from(self.width),
            self.clock.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            &self.frame[4..],
        )
        .map(Some)
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = activity(|env, object| {
            env.call_method(
                object,
                jni_str!("veilCloseCamera"),
                jni_sig!(() -> void),
                &[],
            )?;
            Ok(())
        });
    }
}
