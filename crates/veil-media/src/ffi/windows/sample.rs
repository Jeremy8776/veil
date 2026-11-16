//! Media Foundation samples: copying pictures in, and access units and pictures out.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFSample, MF2DBuffer_LockFlags_Read, MFCreateMemoryBuffer, MFCreateSample,
};
use windows::core::Interface;

use super::step;
use crate::MediaError;
use crate::capture::pack_nv12;

/// Where an NV12 picture sits in a Media Foundation buffer.
#[derive(Clone, Copy, Debug)]
pub(super) struct Nv12Layout {
    /// The picture to keep.
    pub width: u16,
    pub height: u16,
    /// Bytes per row, for a buffer that does not report its own pitch (not 2D).
    pub stride: usize,
    /// Luma rows before the chroma plane starts: the height, or more when a decoder pads it.
    pub alloc_rows: usize,
}

/// Copies `bytes` (a picture or an access unit) into a new sample.
pub(super) fn input_sample(
    bytes: &[u8],
    timestamp_us: u64,
    duration: i64,
) -> Result<IMFSample, MediaError> {
    let len = u32::try_from(bytes.len()).map_err(|_| MediaError::TooLarge)?;
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
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
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

/// Copies the NV12 picture in `sample` into `out`, tightly packed. A 2D buffer's own pitch
/// wins over `layout.stride`. `fail` names the error kind: capture or decoder.
pub(super) fn copy_nv12(
    sample: &IMFSample,
    layout: Nv12Layout,
    out: &mut Vec<u8>,
    fail: fn(&'static str) -> MediaError,
) -> Result<(), MediaError> {
    let Nv12Layout {
        width,
        height,
        stride,
        alloc_rows,
    } = layout;
    // SAFETY: `sample` is live. With one buffer (the normal case) this returns that buffer
    // without copying.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|_| fail("buffer"))?;
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
        let (mut scan0, mut start) = (std::ptr::null_mut(), std::ptr::null_mut());
        let (mut pitch, mut len) = (0i32, 0u32);
        // SAFETY: all four are valid out values. On success the buffer stays locked until
        // Unlock2D, and [`start`, `start + len`) is readable, with `scan0` inside it.
        unsafe {
            two_d.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut scan0,
                &mut pitch,
                &mut start,
                &mut len,
            )
        }
        .map_err(|_| fail("lock picture"))?;
        let offset = (scan0 as usize).checked_sub(start as usize);
        let result = match (offset, usize::try_from(pitch)) {
            (Some(offset), Ok(pitch)) if !scan0.is_null() && offset <= len as usize => {
                // SAFETY: `scan0` is `offset` bytes into the locked buffer of `len` readable
                // bytes (checked above), and the slice is copied before Unlock2D.
                let bytes = unsafe { std::slice::from_raw_parts(scan0, len as usize - offset) };
                pack_nv12(bytes, pitch, alloc_rows, width, height, out)
            }
            // A negative pitch is a bottom-up picture, which NV12 never is.
            _ => Err(fail("picture layout")),
        };
        // SAFETY: the buffer was locked above.
        unsafe { two_d.Unlock2D() }.map_err(|_| fail("unlock picture"))?;
        return result;
    }
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `ptr` and `len` are valid out values. On success `ptr` points to `len` readable
    // bytes until Unlock.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.map_err(|_| fail("lock picture"))?;
    let result = if ptr.is_null() {
        Err(fail("empty picture"))
    } else {
        // SAFETY: `ptr` points to `len` readable bytes while locked, copied before Unlock.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        pack_nv12(bytes, stride, alloc_rows, width, height, out)
    };
    // SAFETY: the buffer was locked above.
    unsafe { buffer.Unlock() }.map_err(|_| fail("unlock picture"))?;
    result
}
