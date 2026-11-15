//! Finding H.264 encoder transforms: hardware ones through `MFTEnumEx`, and Microsoft's
//! software encoder. Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).

use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264EncoderMFT, IMFActivate, IMFTransform, MFMediaType_Video,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_FRIENDLY_NAME_Attribute, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::core::PWSTR;

/// Every hardware H.264 encoder that takes NV12, best first, activated.
pub(super) fn hardware_transforms() -> Vec<(IMFTransform, String, bool)> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos outlive the call, and `list` and `count` are valid out values.
    let found = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            &mut list,
            &mut count,
        )
    };
    if found.is_err() || list.is_null() {
        return Vec::new();
    }
    let mut transforms = Vec::new();
    for index in 0..count as usize {
        // SAFETY: MFTEnumEx returned an array of `count` owned activation pointers. Each is
        // moved out exactly once here, and the array is freed below without touching them.
        let activate = unsafe { list.add(index).read() };
        let Some(activate) = activate else { continue };
        // SAFETY: `activate` is a live activation object; this creates its transform.
        if let Ok(transform) = unsafe { activate.ActivateObject::<IMFTransform>() } {
            transforms.push((transform, friendly_name(&activate), true));
        }
    }
    // SAFETY: the array was allocated by MFTEnumEx with CoTaskMemAlloc; its items were moved
    // out above, so freeing it releases nothing twice.
    unsafe { CoTaskMemFree(Some(list as *const _)) };
    transforms
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: `text` and `len` are valid out values. On success `text` is a CoTaskMemAlloc
    // string we own.
    if unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len) }
        .is_err()
        || text.is_null()
    {
        return String::from("hardware H.264 encoder");
    }
    // SAFETY: `text` is a valid nul-terminated wide string until freed just after.
    let name = unsafe { text.to_string() }.unwrap_or_default();
    // SAFETY: frees the string GetAllocatedString allocated, exactly once.
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    name
}

/// Microsoft's software H.264 encoder, which ships with Windows.
pub(super) fn software_transform() -> Option<IMFTransform> {
    // SAFETY: the class id is a static GUID and no outer object is passed.
    unsafe { CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER) }.ok()
}
