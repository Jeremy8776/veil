//! Screen capture on KDE Plasma (Wayland) through KWin's own interfaces, so Veil can show its
//! own picker there instead of the desktop's chooser.
//!
//! Purpose: KWin gives unsandboxed apps a live window list (`org_kde_plasma_window_management`),
//! streams of any screen or window without a dialog (`zkde_screencast_unstable_v1`) and
//! screenshots into a pipe (D-Bus `org.kde.KWin.ScreenShot2`). [`get`] connects once; the list
//! feeds the picker, screenshots feed its thumbnails and area preview, and a stream feeds a
//! share.
//!
//! Invariants:
//! - Nothing is captured until a screenshot or stream is asked for; pictures stay in memory.
//! - Window titles stay in this process: they are never logged.
//! - Ids are KWin's window ids and Veil's own screen numbers; a closed window's id is never
//!   reused for another window by KWin, and a capture of it fails rather than moving on.
//! - If any of the three is missing (another desktop, a sandbox, an old KWin without the
//!   `.desktop` grant) [`get`] is `None` and the desktop's chooser is used instead.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use ashpd::zbus;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1;
use wayland_protocols_plasma::plasma_window_management::client::{
    org_kde_plasma_window, org_kde_plasma_window_management,
};
use wayland_protocols_plasma::screencast::v1::client::{
    zkde_screencast_stream_unstable_v1 as stream_v1, zkde_screencast_unstable_v1,
};

use super::Grabbed;
use super::wayland::{self, PipeStream};

/// `org_kde_plasma_window_management` state flags.
const MINIMIZED: u32 = 0x2;
const SKIP_TASKBAR: u32 = 0x1000;
const SKIP_SWITCHER: u32 = 0x40000;
/// `zkde_screencast_unstable_v1` pointer mode: drawn into the picture.
const POINTER_EMBEDDED: u32 = 2;
/// How long KWin may take to make a stream.
const STREAM_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Screen(u32),
    Window(u32),
}

struct Window {
    uuid: String,
    title: String,
    pid: u32,
    state: u32,
    ready: bool,
}

struct Output {
    output: wl_output::WlOutput,
    name: String,
}

#[derive(Default)]
struct Model {
    windows: BTreeMap<u32, Window>,
    /// Window uuids from bottom to top.
    stacking: Vec<String>,
    /// By Veil's screen number.
    outputs: BTreeMap<u32, Output>,
    next_output: u32,
    /// Output names, primary first, and the list being received.
    order: Vec<String>,
    order_next: Vec<String>,
}

/// The event handler on the Wayland thread.
struct Handler(Arc<Mutex<Model>>);

fn lock(model: &Mutex<Model>) -> MutexGuard<'_, Model> {
    model.lock().unwrap_or_else(|e| e.into_inner())
}

/// A stream being made: KWin answers with its PipeWire node, or not.
#[derive(Default)]
struct Pending {
    node: Mutex<Option<Result<u32, ()>>>,
    answered: Condvar,
    ended: Arc<AtomicBool>,
}

impl Pending {
    fn answer(&self, node: Result<u32, ()>) {
        let mut slot = self.node.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(node);
        self.answered.notify_all();
    }
}

pub(super) struct Kde {
    conn: Connection,
    queue: QueueHandle<Handler>,
    model: Arc<Mutex<Model>>,
    screencast: zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1,
    alive: Arc<AtomicBool>,
    bus: zbus::Proxy<'static>,
}

/// KWin's interfaces, connected on first use; `None` where they are not all there.
pub(super) fn get() -> Option<&'static Kde> {
    static KDE: OnceLock<Option<Kde>> = OnceLock::new();
    KDE.get_or_init(connect)
        .as_ref()
        .filter(|kde| kde.alive.load(Ordering::Acquire))
}

fn connect() -> Option<Kde> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Handler>(&conn).ok()?;
    let qh = queue.handle();
    let windows: org_kde_plasma_window_management::OrgKdePlasmaWindowManagement =
        globals.bind(&qh, 13..=16, ()).ok()?;
    let screencast = globals.bind(&qh, 1..=4, ()).ok()?;
    let order: Option<kde_output_order_v1::KdeOutputOrderV1> = globals.bind(&qh, 1..=1, ()).ok();
    let model = Arc::new(Mutex::new(Model::default()));
    let mut handler = Handler(model.clone());
    globals.contents().with_list(|list| {
        for global in list.iter().filter(|g| g.interface == "wl_output") {
            bind_output(
                &globals.registry().clone(),
                global.name,
                global.version,
                &qh,
                &model,
            );
        }
    });
    // Windows arrive on the first round trip, their details on the second.
    queue.roundtrip(&mut handler).ok()?;
    queue.roundtrip(&mut handler).ok()?;
    let bus = pollster::block_on(screenshot_bus()).ok()?;
    let alive = Arc::new(AtomicBool::new(true));
    std::thread::Builder::new()
        .name("veil-kwin".into())
        .spawn({
            let alive = alive.clone();
            let (_windows, _order) = (windows, order);
            move || {
                while queue.blocking_dispatch(&mut handler).is_ok() {}
                alive.store(false, Ordering::Release);
            }
        })
        .ok()?;
    Some(Kde {
        conn,
        queue: qh,
        model,
        screencast,
        alive,
        bus,
    })
}

/// The screenshot interface, checked by reading its version.
async fn screenshot_bus() -> zbus::Result<zbus::Proxy<'static>> {
    let session = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new_owned(
        session,
        "org.kde.KWin",
        "/org/kde/KWin/ScreenShot2",
        "org.kde.KWin.ScreenShot2",
    )
    .await?;
    proxy.get_property::<u32>("Version").await?;
    Ok(proxy)
}

fn bind_output(
    registry: &wl_registry::WlRegistry,
    name: u32,
    version: u32,
    qh: &QueueHandle<Handler>,
    model: &Mutex<Model>,
) {
    // Version 4 tells the output's name, which screenshots and the order use.
    if version < 4 {
        return;
    }
    let mut model = lock(model);
    let number = model.next_output;
    model.next_output += 1;
    let output = registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, (number, name));
    model.outputs.insert(
        number,
        Output {
            output,
            name: String::new(),
        },
    );
}

impl Kde {
    /// Screens, primary first, then windows from front to back, leaving out minimised,
    /// untitled, helper windows and those of process `own_pid`.
    pub(super) fn list(&self, own_pid: u32) -> (Vec<u32>, Vec<(u32, String)>) {
        let model = lock(&self.model);
        let rank = |name: &str| {
            model
                .order
                .iter()
                .position(|n| n == name)
                .unwrap_or(usize::MAX)
        };
        let mut screens: Vec<_> = model
            .outputs
            .iter()
            .filter(|(_, o)| !o.name.is_empty())
            .collect();
        screens.sort_by_key(|(number, o)| (rank(&o.name), **number));
        let screens = screens.into_iter().map(|(number, _)| *number).collect();
        let height = |uuid: &str| model.stacking.iter().position(|u| u == uuid);
        let mut windows: Vec<_> = model
            .windows
            .iter()
            .filter(|(_, w)| {
                w.ready
                    && w.pid != own_pid
                    && !w.title.trim().is_empty()
                    && w.state & (MINIMIZED | SKIP_TASKBAR | SKIP_SWITCHER) == 0
            })
            .collect();
        // Topmost first; any not yet in the stacking order go last.
        windows.sort_by_key(|(id, w)| (std::cmp::Reverse(height(&w.uuid)), **id));
        let windows = windows
            .into_iter()
            .map(|(id, w)| (*id, w.title.clone()))
            .collect();
        (screens, windows)
    }

    /// A screenshot of `source`. `Ok(None)` for a minimised window; `Err` if it is gone.
    pub(super) fn screenshot(&self, source: Source) -> Result<Option<Grabbed>, ()> {
        let (method, target) = {
            let model = lock(&self.model);
            match source {
                Source::Screen(number) => (
                    "CaptureScreen",
                    model.outputs.get(&number).ok_or(())?.name.clone(),
                ),
                Source::Window(id) => {
                    let window = model.windows.get(&id).ok_or(())?;
                    if window.state & MINIMIZED != 0 {
                        return Ok(None);
                    }
                    ("CaptureWindow", window.uuid.clone())
                }
            }
        };
        let (mut reader, writer) = std::io::pipe().map_err(|_| ())?;
        // KWin may write more than a pipe holds before it answers, so reading starts first.
        let read = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        });
        let mut options = HashMap::new();
        options.insert("include-cursor", zbus::wire::Variant::from(true));
        let body = (
            target.as_str(),
            options,
            zbus::wire::Fd::from(writer.as_fd()),
        );
        let reply: Result<HashMap<String, zbus::wire::OwnedVariant>, _> =
            pollster::block_on(self.bus.call(method, &body));
        drop(body);
        drop(writer);
        let bytes = read.join().map_err(|_| ())?.map_err(|_| ())?;
        let reply = reply.map_err(|_| ())?;
        let number = |key: &str| reply.get(key).and_then(|v| u32::try_from(v).ok());
        let (width, height, stride, format) = (
            number("width"),
            number("height"),
            number("stride"),
            number("format"),
        );
        let (Some(width), Some(height), Some(stride), Some(format)) =
            (width, height, stride, format)
        else {
            return Err(());
        };
        let bgr = qimage_is_bgr(format).ok_or(())?;
        wayland::pack(&bytes, width, height, i64::from(stride), bgr)
            .map(Some)
            .ok_or(())
    }

    /// A live stream of `source`, pointer drawn in. Ends when KWin closes it.
    pub(super) fn stream(&self, source: Source) -> Result<PipeStream, ()> {
        let pending = Arc::new(Pending::default());
        let stream = {
            let model = lock(&self.model);
            match source {
                Source::Screen(number) => {
                    let output = &model.outputs.get(&number).ok_or(())?.output;
                    self.screencast.stream_output(
                        output,
                        POINTER_EMBEDDED,
                        &self.queue,
                        pending.clone(),
                    )
                }
                Source::Window(id) => {
                    let uuid = model.windows.get(&id).ok_or(())?.uuid.clone();
                    self.screencast.stream_window(
                        uuid,
                        POINTER_EMBEDDED,
                        &self.queue,
                        pending.clone(),
                    )
                }
            }
        };
        let _ = self.conn.flush();
        let node = {
            let slot = pending.node.lock().unwrap_or_else(|e| e.into_inner());
            let (slot, _) = pending
                .answered
                .wait_timeout_while(slot, STREAM_WAIT, |node| node.is_none())
                .unwrap_or_else(|e| e.into_inner());
            *slot
        };
        let conn = self.conn.clone();
        let close = move || {
            stream.close();
            let _ = conn.flush();
        };
        match node {
            Some(Ok(node)) => {
                wayland::open_node(node, pending.ended.clone(), Box::new(close)).map_err(|_| ())
            }
            _ => {
                close();
                Err(())
            }
        }
    }
}

/// True if a `QImage::Format` keeps blue first in memory, false if red; `None` for others.
fn qimage_is_bgr(format: u32) -> Option<bool> {
    match format {
        // RGB32, ARGB32 and its premultiplied form are 0xAARRGGBB words, little endian here.
        4..=6 => Some(cfg!(target_endian = "little")),
        // RGBX8888, RGBA8888 and its premultiplied form are bytes in that order.
        16..=18 => Some(false),
        _ => None,
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Handler {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => bind_output(registry, name, version, qh, &state.0),
            wl_registry::Event::GlobalRemove { name } => {
                lock(&state.0).outputs.retain(|_, o| {
                    let gone = o.output.data::<(u32, u32)>().is_some_and(|d| d.1 == name);
                    if gone {
                        o.output.release();
                    }
                    !gone
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, (u32, u32)> for Handler {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &(number, _): &(u32, u32),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(output) = lock(&state.0).outputs.get_mut(&number)
        {
            output.name = name;
        }
    }
}

impl Dispatch<kde_output_order_v1::KdeOutputOrderV1, ()> for Handler {
    fn event(
        state: &mut Self,
        _: &kde_output_order_v1::KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut model = lock(&state.0);
        match event {
            kde_output_order_v1::Event::Output { output_name } => {
                model.order_next.push(output_name)
            }
            kde_output_order_v1::Event::Done => model.order = std::mem::take(&mut model.order_next),
            _ => {}
        }
    }
}

impl Dispatch<org_kde_plasma_window_management::OrgKdePlasmaWindowManagement, ()> for Handler {
    fn event(
        state: &mut Self,
        manager: &org_kde_plasma_window_management::OrgKdePlasmaWindowManagement,
        event: org_kde_plasma_window_management::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window_management::Event;
        let mut model = lock(&state.0);
        match event {
            Event::WindowWithUuid { id, uuid } => {
                manager.get_window_by_uuid(uuid.clone(), qh, id);
                let window = Window {
                    uuid,
                    title: String::new(),
                    pid: 0,
                    state: 0,
                    ready: false,
                };
                model.windows.insert(id, window);
            }
            Event::StackingOrderUuidChanged { uuids } => {
                model.stacking = uuids.split(';').map(str::to_string).collect();
            }
            _ => {}
        }
    }
}

impl Dispatch<org_kde_plasma_window::OrgKdePlasmaWindow, u32> for Handler {
    fn event(
        state: &mut Self,
        window: &org_kde_plasma_window::OrgKdePlasmaWindow,
        event: org_kde_plasma_window::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window::Event;
        let mut model = lock(&state.0);
        if let Event::Unmapped = event {
            model.windows.remove(id);
            window.destroy();
            return;
        }
        let Some(found) = model.windows.get_mut(id) else {
            return;
        };
        match event {
            Event::TitleChanged { title } => found.title = title,
            Event::StateChanged { flags } => found.state = flags,
            Event::PidChanged { pid } => found.pid = pid,
            Event::InitialState => found.ready = true,
            _ => {}
        }
    }
}

impl Dispatch<stream_v1::ZkdeScreencastStreamUnstableV1, Arc<Pending>> for Handler {
    fn event(
        _: &mut Self,
        _: &stream_v1::ZkdeScreencastStreamUnstableV1,
        event: stream_v1::Event,
        pending: &Arc<Pending>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            stream_v1::Event::Created { node } => pending.answer(Ok(node)),
            stream_v1::Event::Failed { .. } => pending.answer(Err(())),
            stream_v1::Event::Closed => {
                pending.ended.store(true, Ordering::Release);
                pending.answer(Err(()));
            }
            _ => {}
        }
    }
}

delegate_noop!(Handler: ignore zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn qimage_formats_map_to_their_byte_order() {
        assert_eq!(qimage_is_bgr(5), Some(cfg!(target_endian = "little")));
        assert_eq!(qimage_is_bgr(16), Some(false));
        assert_eq!(qimage_is_bgr(13), None, "3-byte RGB888 is refused");
    }

    /// Reads this desktop's window list and one screenshot; nothing is kept or sent.
    #[test]
    #[ignore = "needs KDE Plasma on Wayland"]
    fn kwin_lists_and_captures() {
        let kde = get().unwrap();
        let (screens, windows) = kde.list(std::process::id());
        println!("{} screens, {} windows", screens.len(), windows.len());
        let shot = kde.screenshot(Source::Screen(screens[0])).unwrap().unwrap();
        println!("screen {} x {}", shot.width, shot.height);
        if let Some(&(id, _)) = windows.first() {
            let shot = kde.screenshot(Source::Window(id)).unwrap().unwrap();
            println!("window {} x {}", shot.width, shot.height);
        }
        let stream = kde.stream(Source::Screen(screens[0])).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        assert!(stream.newer_than(0).is_some(), "the stream sends pictures");
        drop(stream);
    }
}
