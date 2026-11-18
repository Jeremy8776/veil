//! A live picture stream of one screen or window, through Windows Graphics Capture.
//! Part of the Windows `ffi` module; the same rules apply (see `mod.rs`).
//!
//! Purpose: taking a full screenshot for every picture limits how smooth a share can be.
//! Windows instead hands over a new picture when the source changes, so reading the newest one
//! costs one copy from the graphics card. The mouse pointer is included.
//!
//! Invariants:
//! - Only the newest waiting picture is read; older ones are released unread.
//! - [`ScreenStream::next`] gives `None` when nothing has changed, and fails once the source
//!   has closed. It never moves to another source.
//! - Every mapped texture is unmapped before its pixels are used, and each row is read within
//!   the pitch Windows reports.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
    GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, factory};

use crate::MediaError;
use crate::screen::{Grabbed, ScreenSource};

const UNAVAILABLE: MediaError = MediaError::Unavailable("screen stream");
const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// One picture being read and one being filled.
const BUFFERS: i32 = 2;

fn failed(_: windows::core::Error) -> MediaError {
    UNAVAILABLE
}

pub struct ScreenStream {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    capture_device: IDirect3DDevice,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    closed: Arc<AtomicBool>,
    closed_token: i64,
    size: Cell<SizeInt32>,
    /// The texture pictures are copied into, with its width and height.
    staging: RefCell<Option<(ID3D11Texture2D, u32, u32)>>,
}

impl ScreenStream {
    pub fn open(source: ScreenSource) -> Result<Self, MediaError> {
        if !GraphicsCaptureSession::IsSupported().map_err(failed)? {
            return Err(UNAVAILABLE);
        }
        let (device, context) = create_device()?;
        let dxgi: IDXGIDevice = device.cast().map_err(failed)?;
        // SAFETY: `dxgi` is a live device, and the call only reads it to wrap it.
        let wrapped = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.map_err(failed)?;
        let capture_device: IDirect3DDevice = wrapped.cast().map_err(failed)?;
        let item = capture_item(source)?;
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let closed_token = item
            .Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                move |_, _| {
                    flag.store(true, Ordering::Release);
                    Ok(())
                },
            ))
            .map_err(failed)?;
        let size = item.Size().map_err(failed)?;
        let pool =
            Direct3D11CaptureFramePool::CreateFreeThreaded(&capture_device, FORMAT, BUFFERS, size)
                .map_err(failed)?;
        let session = pool.CreateCaptureSession(&item).map_err(failed)?;
        // Both are best effort: older versions of Windows do not have them.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);
        session.StartCapture().map_err(failed)?;
        Ok(Self {
            device,
            context,
            capture_device,
            item,
            pool,
            session,
            closed,
            closed_token,
            size: Cell::new(size),
            staging: RefCell::new(None),
        })
    }

    /// The newest picture since the last call, or `None` if nothing has changed.
    pub fn next(&self) -> Result<Option<Grabbed>, MediaError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(UNAVAILABLE);
        }
        let mut newest: Option<Direct3D11CaptureFrame> = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(older) = newest.replace(frame) {
                older.Close().map_err(failed)?;
            }
        }
        let Some(frame) = newest else { return Ok(None) };
        let content = frame.ContentSize().map_err(failed)?;
        let picture = self.read(&frame, content);
        frame.Close().map_err(failed)?;
        // A resized source needs buffers of its new size from the next picture on.
        if content != self.size.get() && content.Width > 0 && content.Height > 0 {
            self.pool
                .Recreate(&self.capture_device, FORMAT, BUFFERS, content)
                .map_err(failed)?;
            self.size.set(content);
        }
        picture
    }

    fn read(
        &self,
        frame: &Direct3D11CaptureFrame,
        content: SizeInt32,
    ) -> Result<Option<Grabbed>, MediaError> {
        let access: IDirect3DDxgiInterfaceAccess =
            frame.Surface().map_err(failed)?.cast().map_err(failed)?;
        // SAFETY: `access` is a live interface of the frame's surface, and the texture it
        // returns holds its own reference.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(failed)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a valid place for the texture to describe itself.
        unsafe { texture.GetDesc(&mut desc) };
        let width = u32::try_from(content.Width).unwrap_or(0).min(desc.Width);
        let height = u32::try_from(content.Height).unwrap_or(0).min(desc.Height);
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let staging = self.staging_texture(desc, width, height)?;
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        let target: ID3D11Resource = staging.cast().map_err(failed)?;
        let from: ID3D11Resource = texture.cast().map_err(failed)?;
        // SAFETY: both textures are live and use one format, and `region` was limited to the
        // size of each.
        unsafe {
            self.context
                .CopySubresourceRegion(&target, 0, 0, 0, 0, &from, 0, Some(&region));
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `target` is a staging texture made for CPU reads, and `mapped` is a valid
        // out pointer.
        unsafe { self.context.Map(&target, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(failed)?;
        let (w, h, pitch) = (width as usize, height as usize, mapped.RowPitch as usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        if pitch >= w * 4 {
            for row in 0..h {
                // SAFETY: while mapped, `pData` points at `h` rows `pitch` bytes apart, and
                // each row holds at least `w * 4` readable bytes (checked above).
                let bgra = unsafe {
                    slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * pitch), w * 4)
                };
                append_as_rgba(bgra, &mut rgba);
            }
        }
        // SAFETY: `target` was mapped above, and nothing reads the mapping after this.
        unsafe { self.context.Unmap(&target, 0) };
        if rgba.len() != w * h * 4 {
            return Err(UNAVAILABLE);
        }
        Ok(Some(Grabbed {
            width,
            height,
            rgba,
        }))
    }

    /// The texture to copy into, made again only when the size changes.
    fn staging_texture(
        &self,
        source: D3D11_TEXTURE2D_DESC,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, MediaError> {
        let mut slot = self.staging.borrow_mut();
        if let Some((texture, w, h)) = slot.as_ref()
            && (*w, *h) == (width, height)
        {
            return Ok(texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..source
        };
        let mut texture = None;
        // SAFETY: `desc` is complete, no initial data is given, and `texture` is a valid out
        // pointer.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(failed)?;
        let texture = texture.ok_or(UNAVAILABLE)?;
        *slot = Some((texture.clone(), width, height));
        Ok(texture)
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), MediaError> {
    let (mut device, mut context) = (None, None);
    // SAFETY: the out pointers are valid for the call, and every other argument is a plain
    // value or absent.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(failed)?;
    Ok((device.ok_or(UNAVAILABLE)?, context.ok_or(UNAVAILABLE)?))
}

fn capture_item(source: ScreenSource) -> Result<GraphicsCaptureItem, MediaError> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(failed)?;
    // Ids are the low 32 bits of the handle, which are all of its meaningful bits. SAFETY:
    // handles are passed by value, and Windows refuses one that is no longer valid.
    unsafe {
        match source {
            ScreenSource::Monitor(id) => {
                interop.CreateForMonitor(HMONITOR(id as usize as *mut c_void))
            }
            ScreenSource::Window(id) => interop.CreateForWindow(HWND(id as usize as *mut c_void)),
        }
    }
    .map_err(failed)
}

/// Adds BGRA pixels to `out` as opaque RGBA.
fn append_as_rgba(bgra: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + bgra.len(), 255);
    let (from, _) = bgra.as_chunks::<4>();
    let (to, _) = out[start..].as_chunks_mut::<4>();
    for (to, from) in to.iter_mut().zip(from) {
        to[0] = from[2];
        to[1] = from[1];
        to[2] = from[0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blue_and_red_swap_and_pixels_become_opaque() {
        let mut out = vec![9];
        append_as_rgba(&[1, 2, 3, 0, 4, 5, 6, 7], &mut out);
        assert_eq!(out, [9, 3, 2, 1, 255, 6, 5, 4, 255]);
    }
}
