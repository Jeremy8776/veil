//! Listing microphones and speakers, and opening one by id, through the WASAPI device
//! enumerator. Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Invariants:
//! - Only active devices are listed (plugged in and enabled).
//! - Every string Windows allocates (device id, friendly name) is copied and freed exactly
//!   once, and each property value is cleared after use.
//! - `None` for a device means the default communications device, which follows Windows'
//!   own setting.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, EDataFlow, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
    eCommunications, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PropVariantClear, PropVariantToBoolean, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{GUID, HSTRING, PWSTR};

use super::Runtime;
use crate::MediaError;

/// A microphone or speaker: its name for people, its id for [`super::WasapiCapture::open`]
/// and [`super::WasapiPlayback::open`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub id: String,
}

/// Every active microphone.
pub fn microphones() -> Result<Vec<AudioDevice>, MediaError> {
    list(eCapture)
}

/// Every active speaker or headset output.
pub fn speakers() -> Result<Vec<AudioDevice>, MediaError> {
    list(eRender)
}

fn list(flow: EDataFlow) -> Result<Vec<AudioDevice>, MediaError> {
    let _runtime = Runtime::start()?;
    let devices = enumerator()?;
    // SAFETY: `devices` is live; the flow and state mask are plain values.
    let collection = unsafe { devices.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
        .map_err(|_| MediaError::Unavailable("audio device list"))?;
    // SAFETY: `collection` is live.
    let count = unsafe { collection.GetCount() }.unwrap_or(0);
    let mut out = Vec::new();
    for index in 0..count {
        // SAFETY: `index` is below the count the collection reported.
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        if let Some(id) = device_id(&device) {
            let name = friendly_name(&device).unwrap_or_else(|| id.clone());
            out.push(AudioDevice { name, id });
        }
    }
    Ok(out)
}

pub(super) fn enumerator() -> Result<IMMDeviceEnumerator, MediaError> {
    // SAFETY: the class id is a static GUID and no outer object is passed.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|_| MediaError::Unavailable("audio device list"))
}

/// The device with this id, or the default communications device for `None`.
pub(super) fn find(
    devices: &IMMDeviceEnumerator,
    flow: EDataFlow,
    id: Option<&str>,
) -> Result<IMMDevice, MediaError> {
    let found = match id {
        // SAFETY: `devices` is live and the id is a nul-terminated wide string that outlives
        // the call.
        Some(id) => unsafe { devices.GetDevice(&HSTRING::from(id)) },
        // SAFETY: `devices` is live; the flow and role are plain values.
        None => unsafe { devices.GetDefaultAudioEndpoint(flow, eCommunications) },
    };
    found.map_err(|_| MediaError::Unavailable("audio device"))
}

fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: `device` is live. On success the id is a CoTaskMemAlloc string we own.
    let id = unsafe { device.GetId() }.ok()?;
    take_string(id)
}

/// `PKEY_Devices_AudioDevice_RawProcessingSupported`, written here to avoid a whole
/// `windows` feature for one key.
const RAW_SUPPORTED: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x8943b373_388c_4395_b557_bc6dbaffafdb),
    pid: 2,
};

/// Whether the device's driver says it offers raw mode (a stream without its voice effects).
/// Asking for raw mode on a device without it may succeed and still apply the effects.
pub(super) fn raw_supported(device: &IMMDevice) -> bool {
    // SAFETY: `device` is live; read access only.
    let Ok(store) = (unsafe { device.OpenPropertyStore(STGM_READ) }) else {
        return false;
    };
    // SAFETY: `store` is live and the key is a static PROPERTYKEY.
    let Ok(mut value) = (unsafe { store.GetValue(&RAW_SUPPORTED) }) else {
        return false;
    };
    // SAFETY: `value` is a valid PROPVARIANT returned above; an empty one is an error.
    let supported = unsafe { PropVariantToBoolean(&value) }.is_ok_and(|b| b.as_bool());
    // SAFETY: clears the value GetValue returned, exactly once.
    let _ = unsafe { PropVariantClear(&mut value) };
    supported
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: `device` is live; read access only.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    // SAFETY: `store` is live and the key is a static PROPERTYKEY.
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    // SAFETY: `value` is a valid PROPVARIANT returned above. On success the string is a
    // CoTaskMemAlloc string we own.
    let text = unsafe { PropVariantToStringAlloc(&value) }.ok();
    // SAFETY: clears the value GetValue returned, exactly once.
    let _ = unsafe { PropVariantClear(&mut value) };
    take_string(text?)
}

/// Copies a CoTaskMemAlloc wide string and frees it.
fn take_string(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: `text` is a valid nul-terminated wide string until freed just after.
    let value = unsafe { text.to_string() }.ok();
    // SAFETY: frees the string Windows allocated, exactly once.
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    value
}
