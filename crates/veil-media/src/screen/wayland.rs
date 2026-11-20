//! Screen capture on Wayland: the desktop's screen cast portal picks the source and PipeWire
//! streams its pictures.
//!
//! Purpose: on Wayland an app may not list or capture other windows itself. [`choose`] asks the
//! desktop to show its own chooser, and on a pick opens that one stream. Pictures arrive on a
//! thread of its own and wait in a single slot, newest wins, until they are read.
//!
//! Invariants:
//! - Pictures stay in memory; nothing is written to disk.
//! - Dropping the last handle to a [`PortalStream`] stops its PipeWire loop, joins its thread
//!   and closes the portal session, so the desktop's sharing indicator goes away.
//! - A stream the desktop ends (its own stop control, the window closed) reads as ended; it is
//!   never reopened or replaced by another source.
//! - The pointer is drawn into the pictures where the desktop offers that.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{ResponseError, Session};
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::pod::{self, Pod, serialize::PodSerializer};
use pipewire::spa::utils::{Direction, SpaTypes};
use pipewire::stream::{StreamFlags, StreamRc, StreamState};
use pipewire::{context::ContextRc, main_loop::MainLoopRc};

use super::Grabbed;

/// What the stream thread shares with its handles.
#[derive(Default)]
struct Shared {
    /// The newest picture and its number. The number only grows.
    latest: Mutex<(u64, Option<Arc<Grabbed>>)>,
    ended: AtomicBool,
}

/// One source the person picked in the desktop's chooser, streaming until dropped.
pub(super) struct PortalStream {
    shared: Arc<Shared>,
    quit: pipewire::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

/// Why no stream was opened.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoStream {
    /// The person closed the desktop's chooser without picking.
    Cancelled,
    /// The portal or PipeWire failed. The text names the step, never a window title.
    Failed(&'static str),
}

/// Shows the desktop's chooser and opens the picked source. Blocks until the person picks or
/// cancels, so it must not run on the UI thread.
pub(super) fn choose() -> Result<PortalStream, NoStream> {
    let shared = Arc::new(Shared::default());
    let (reply, answer) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("veil-screencast".into())
        .spawn({
            let shared = shared.clone();
            move || run(shared, &reply)
        })
        .map_err(|_| NoStream::Failed("thread"))?;
    match answer.recv() {
        Ok(Ok(quit)) => Ok(PortalStream {
            shared,
            quit,
            thread: Some(thread),
        }),
        Ok(Err(why)) => {
            let _ = thread.join();
            Err(why)
        }
        Err(_) => {
            let _ = thread.join();
            Err(NoStream::Failed("stream thread"))
        }
    }
}

impl PortalStream {
    /// The newest picture if its number is above `seen`, with its number.
    pub(super) fn newer_than(&self, seen: u64) -> Option<(u64, Arc<Grabbed>)> {
        let latest = self.shared.latest.lock().unwrap_or_else(|e| e.into_inner());
        match &latest.1 {
            Some(picture) if latest.0 > seen => Some((latest.0, picture.clone())),
            _ => None,
        }
    }

    pub(super) fn has_ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
    }
}

impl Drop for PortalStream {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Reply = mpsc::Sender<Result<pipewire::channel::Sender<()>, NoStream>>;

/// The stream thread: portal handshake, then PipeWire until told to quit, then closing up.
fn run(shared: Arc<Shared>, reply: &Reply) {
    let (portal, session) = match pollster::block_on(open_session()) {
        Ok(opened) => opened,
        Err(_) => {
            let _ = reply.send(Err(NoStream::Failed("portal")));
            return;
        }
    };
    match pollster::block_on(pick(&portal, &session)) {
        Ok((fd, node)) => {
            if let Err(step) = stream(fd, node, &shared, reply) {
                let _ = reply.send(Err(NoStream::Failed(step)));
            }
        }
        Err(why) => {
            let _ = reply.send(Err(why));
        }
    }
    shared.ended.store(true, Ordering::Release);
    let _ = pollster::block_on(session.close());
}

async fn open_session() -> ashpd::Result<(Screencast, Session<Screencast>)> {
    let portal = Screencast::new().await?;
    let session = portal.create_session(Default::default()).await?;
    Ok((portal, session))
}

/// Asks the desktop for one screen or window. Returns the PipeWire connection and node.
async fn pick(
    portal: &Screencast,
    session: &Session<Screencast>,
) -> Result<(OwnedFd, u32), NoStream> {
    let refused = |e: ashpd::Error| match e {
        ashpd::Error::Response(ResponseError::Cancelled) => NoStream::Cancelled,
        _ => NoStream::Failed("portal"),
    };
    let cursors = portal
        .available_cursor_modes()
        .await
        .unwrap_or(CursorMode::empty());
    let cursor = cursors
        .contains(CursorMode::EMBEDDED)
        .then_some(CursorMode::EMBEDDED);
    let options = SelectSourcesOptions::default()
        .set_sources(SourceType::MONITOR | SourceType::WINDOW)
        .set_multiple(false)
        .set_cursor_mode(cursor);
    portal
        .select_sources(session, options)
        .await
        .map_err(refused)?
        .response()
        .map_err(refused)?;
    let streams = portal
        .start(session, None, Default::default())
        .await
        .map_err(refused)?
        .response()
        .map_err(refused)?;
    let node = streams
        .streams()
        .first()
        .ok_or(NoStream::Cancelled)?
        .pipe_wire_node_id();
    let fd = portal
        .open_pipe_wire_remote(session, Default::default())
        .await
        .map_err(refused)?;
    Ok((fd, node))
}

/// Runs PipeWire for `node` until the quit message. Sends the quit sender once streaming is
/// set up; an error before then names the step.
fn stream(fd: OwnedFd, node: u32, shared: &Arc<Shared>, reply: &Reply) -> Result<(), &'static str> {
    pipewire::init();
    let main_loop = MainLoopRc::new(None).map_err(|_| "pipewire loop")?;
    let context = ContextRc::new(&main_loop, None).map_err(|_| "pipewire context")?;
    let core = context
        .connect_fd_rc(fd, None)
        .map_err(|_| "pipewire connect")?;
    let stream = StreamRc::new(
        core,
        "Veil screen share",
        pipewire::properties::properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|_| "pipewire stream")?;
    let _listener = stream
        .add_local_listener_with_user_data(VideoInfoRaw::default())
        .param_changed(|_, format, id, param| {
            let Some(param) = param.filter(|_| id == ParamType::Format.as_raw()) else {
                return;
            };
            if let Ok((MediaType::Video, MediaSubtype::Raw)) = format_utils::parse_format(param) {
                let _ = format.parse(param);
            }
        })
        .state_changed({
            let shared = shared.clone();
            move |_, _, _, now| {
                if matches!(now, StreamState::Error(_) | StreamState::Unconnected) {
                    shared.ended.store(true, Ordering::Release);
                }
            }
        })
        .process({
            let shared = shared.clone();
            move |stream, format| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let (offset, stride) = (data.chunk().offset() as usize, data.chunk().stride());
                let size = format.size();
                let picture = data.data().and_then(|bytes| {
                    to_rgba(
                        format.format(),
                        bytes.get(offset..)?,
                        size.width,
                        size.height,
                        stride,
                    )
                });
                if let Some(picture) = picture {
                    let mut latest = shared.latest.lock().unwrap_or_else(|e| e.into_inner());
                    *latest = (latest.0 + 1, Some(Arc::new(picture)));
                }
            }
        })
        .register()
        .map_err(|_| "pipewire listener")?;
    let formats = pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
    );
    let bytes = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pod::Value::Object(formats),
    )
    .map_err(|_| "pipewire format")?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&bytes).ok_or("pipewire format")?];
    stream
        .connect(
            Direction::Input,
            Some(node),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|_| "pipewire stream connect")?;
    let (quit, quit_asked) = pipewire::channel::channel();
    let _attached = quit_asked.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        move |()| main_loop.quit()
    });
    reply.send(Ok(quit)).map_err(|_| "nobody waiting")?;
    main_loop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// Copies a `width` x `height` picture in `format`, rows `stride` bytes apart, to packed RGBA.
/// `None` for another format or too little data.
fn to_rgba(
    format: VideoFormat,
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: i32,
) -> Option<Grabbed> {
    let swap = match format {
        VideoFormat::BGRx | VideoFormat::BGRA => true,
        VideoFormat::RGBx | VideoFormat::RGBA => false,
        _ => return None,
    };
    let (w, h) = (width as usize, height as usize);
    let stride = usize::try_from(stride).ok().filter(|&s| s >= w * 4)?;
    if w == 0 || h == 0 || bytes.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in bytes.chunks(stride).take(h) {
        let (pixels, _) = row[..w * 4].as_chunks::<4>();
        rgba.extend(pixels.iter().flat_map(|p| {
            let [r, g, b] = if swap {
                [p[2], p[1], p[0]]
            } else {
                [p[0], p[1], p[2]]
            };
            [r, g, b, 255]
        }));
    }
    Some(Grabbed {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn threads() -> Vec<String> {
        let names = std::fs::read_dir("/proc/self/task").unwrap().map(|task| {
            std::fs::read_to_string(task.unwrap().path().join("comm"))
                .unwrap()
                .trim()
                .to_string()
        });
        names.collect()
    }

    /// Opens the desktop's chooser: pick a screen. Pictures stay in memory.
    #[test]
    #[ignore = "needs a Wayland desktop and a person to pick"]
    fn a_picked_screen_streams_and_closes_cleanly() {
        let stream = choose().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        let (count, picture) = stream.newer_than(0).unwrap();
        println!("{count} pictures, {} x {}", picture.width, picture.height);
        assert!(!stream.has_ended());
        drop(stream);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let left = threads();
        println!("threads left: {left:?}");
        assert!(
            !left.iter().any(|name| name == "veil-screencast"),
            "the stream thread is gone"
        );
    }

    #[test]
    fn bgr_pictures_are_swapped_padding_skipped_and_made_opaque() {
        // 2 x 2, one byte of padding per row past the 8 bytes of pixels.
        let bytes = [1, 2, 3, 0, 4, 5, 6, 0, 99, 7, 8, 9, 0, 10, 11, 12, 0];
        let picture = to_rgba(VideoFormat::BGRx, &bytes, 2, 2, 9).unwrap();
        assert_eq!(
            picture.rgba,
            [3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
        let picture = to_rgba(VideoFormat::RGBA, &bytes, 2, 2, 9).unwrap();
        assert_eq!(&picture.rgba[..4], [1, 2, 3, 255]);
    }

    #[test]
    fn short_data_odd_strides_and_other_formats_are_refused() {
        let bytes = [0; 16];
        assert!(to_rgba(VideoFormat::BGRx, &bytes, 2, 2, 8).is_some());
        assert!(
            to_rgba(VideoFormat::BGRx, &bytes, 2, 3, 8).is_none(),
            "too short"
        );
        assert!(
            to_rgba(VideoFormat::BGRx, &bytes, 2, 2, 4).is_none(),
            "stride below width"
        );
        assert!(
            to_rgba(VideoFormat::BGRx, &bytes, 2, 2, -8).is_none(),
            "upside down"
        );
        assert!(
            to_rgba(VideoFormat::I420, &bytes, 2, 2, 8).is_none(),
            "planar"
        );
    }
}
