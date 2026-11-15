//! Media Foundation samples: copying pictures in and access units out.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).

use windows::Win32::Media::MediaFoundation::{IMFSample, MFCreateMemoryBuffer, MFCreateSample};

use super::step;
use crate::MediaError;

/// Copies one picture into a new sample.
pub(super) fn input_sample(
    nv12: &[u8],
    timestamp_us: u64,
    duration: i64,
) -> Result<IMFSample, MediaError> {
    let len = u32::try_from(nv12.len()).map_err(|_| MediaError::TooLarge)?;
    // SAFETY: no pointers are passed; the results are owned interfaces.
    let (buffer, sample) = unsafe {
        (
            MFCreateMemoryBuffer(len).map_err(step("buffer"))?,
            MFCreateSample().map_err(step("sample"))?,
        )
    };
    let mut ptr = std::ptr::null_mut();
    let mut max = 0u32;
    // SAFETY: `ptr` and `max` are valid out values. On success the buffer stays locked until
    // Unlock below, and `ptr` points to at least `max` writable bytes.
    unsafe { buffer.Lock(&mut ptr, Some(&mut max), None) }.map_err(step("lock buffer"))?;
    if max < len || ptr.is_null() {
        // SAFETY: the buffer was locked above.
        let _ = unsafe { buffer.Unlock() };
        return Err(MediaError::Encoder("buffer too small"));
    }
    // SAFETY: `ptr` points to at least `len` writable bytes while locked (checked above), and a
    // fresh Media Foundation buffer cannot overlap the caller's slice.
    unsafe { std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len()) };
    let timestamp = i64::try_from(timestamp_us.saturating_mul(10)).unwrap_or(i64::MAX);
    // SAFETY: the buffer was locked above; the rest take no pointers but owned interfaces.
    unsafe {
        buffer.Unlock().map_err(step("unlock buffer"))?;
        buffer
            .SetCurrentLength(len)
            .map_err(step("buffer length"))?;
        sample.AddBuffer(&buffer).map_err(step("sample"))?;
        sample
            .SetSampleTime(timestamp)
            .map_err(step("sample time"))?;
        sample
            .SetSampleDuration(duration)
            .map_err(step("sample time"))?;
    }
    Ok(sample)
}

/// A sample with one empty buffer, for a transform that does not allocate its own.
pub(super) fn empty_sample(size: u32) -> Result<IMFSample, MediaError> {
    // SAFETY: no pointers are passed; the results are owned interfaces.
    unsafe {
        let buffer = MFCreateMemoryBuffer(size).map_err(step("buffer"))?;
        let sample = MFCreateSample().map_err(step("sample"))?;
        sample.AddBuffer(&buffer).map_err(step("sample"))?;
        Ok(sample)
    }
}

/// Copies an output sample's bytes out.
pub(super) fn read_sample(sample: &IMFSample) -> Result<Vec<u8>, MediaError> {
    // SAFETY: `sample` is a live sample; the result is an owned buffer.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(step("output buffer"))?;
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `ptr` and `len` are valid out values. On success `ptr` points to `len` readable
    // bytes until Unlock.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.map_err(step("lock output"))?;
    let bytes = if ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: `ptr` points to `len` readable bytes while the buffer is locked, and the
        // slice is copied before Unlock.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
    };
    // SAFETY: the buffer was locked above.
    unsafe { buffer.Unlock() }.map_err(step("unlock output"))?;
    Ok(bytes)
}
