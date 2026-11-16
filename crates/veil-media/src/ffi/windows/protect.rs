//! Sealing small secrets to the signed-in Windows user with DPAPI (`CryptProtectData`).
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`). It lives here
//! because Veil keeps all of its `unsafe` code in this module, not because it is media.
//!
//! Purpose: `veil-app` saves the device key and its list of verified keys on disk. Sealed
//! with DPAPI, the files open only for the same Windows user (on the same machine, unless
//! the account roams), so copying them to another computer gives nothing.
//!
//! Invariants:
//! - `label` is mixed in as DPAPI's extra entropy, so a blob sealed for one purpose does not
//!   open for another.
//! - Windows never shows a prompt (`CRYPTPROTECT_UI_FORBIDDEN`).
//! - Every buffer Windows allocates is copied out and freed exactly once with `LocalFree`.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

use crate::MediaError;

/// Largest secret this seals. The app's files are a few kilobytes.
pub const MAX_PROTECTED: usize = 1 << 20;

/// Seals `secret` so only this Windows user can open it, with [`unprotect`] and the same
/// `label`.
pub fn protect(secret: &[u8], label: &[u8]) -> Result<Vec<u8>, MediaError> {
    if secret.len() > MAX_PROTECTED {
        return Err(MediaError::TooLarge);
    }
    let input = blob(secret)?;
    let entropy = blob(label)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` and `entropy` point into slices that outlive the call, with their true
    // lengths, and DPAPI only reads them. `output` is a valid out pointer; on success DPAPI
    // fills it with a buffer it allocated, which `take` copies and frees.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| MediaError::Unavailable("DPAPI could not seal the data"))?;
    Ok(take(output))
}

/// Opens what [`protect`] sealed with the same `label`. Fails for another Windows user,
/// another label, or changed bytes.
pub fn unprotect(sealed: &[u8], label: &[u8]) -> Result<Vec<u8>, MediaError> {
    if sealed.len() > MAX_PROTECTED * 2 {
        return Err(MediaError::TooLarge);
    }
    let input = blob(sealed)?;
    let entropy = blob(label)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`. No description is asked for, so DPAPI allocates nothing else.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| MediaError::Malformed("the sealed data does not open for this Windows user"))?;
    Ok(take(output))
}

/// A read-only view of `data` in the shape DPAPI takes.
fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, MediaError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| MediaError::TooLarge)?,
        // DPAPI takes a `*mut` but never writes through an input blob.
        pbData: data.as_ptr().cast_mut(),
    })
}

/// Copies DPAPI's output buffer and frees it.
fn take(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if output.pbData.is_null() {
        return Vec::new();
    }
    let len = usize::try_from(output.cbData).unwrap_or(0);
    // SAFETY: on success DPAPI set `pbData` to a buffer of `cbData` bytes that it allocated
    // with `LocalAlloc` and that nothing else references.
    let copied = unsafe { std::slice::from_raw_parts(output.pbData, len) }.to_vec();
    // SAFETY: the buffer came from DPAPI's `LocalAlloc`, is freed here once, and is not
    // used after.
    unsafe {
        LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    copied
}
