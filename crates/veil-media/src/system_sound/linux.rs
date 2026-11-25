//! Linux computer sound through PipeWire, linked app by app so Veil's own playback stays out.
//!
//! Purpose: [`PipeWireSound`] opens a capture stream that the session manager does not connect
//! (`node.autoconnect` off) and links every app's playback stream to it itself, except Veil's
//! own (`graph.rs`). New apps are linked as they start. PipeWire converts to 48 kHz float
//! stereo.
//!
//! The desktop's ScreenCast portal carries pictures only, so sound comes from the session's
//! PipeWire directly. That needs no extra permission outside a sandbox.
//!
//! Invariants:
//! - PipeWire runs on a thread of its own and hands sound over through a `Feed`; the reader
//!   never waits on PipeWire.
//! - Links are made with `object.linger` off, so they go when the capture ends or Veil exits.
//! - Dropping the capture quits PipeWire's loop and waits for its thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use pipewire::link::Link;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::pod::{self, Pod, serialize::PodSerializer};
use pipewire::spa::utils::{Direction, SpaTypes};
use pipewire::stream::{StreamFlags, StreamRc, StreamState};
use pipewire::types::ObjectType;
use pipewire::{context::ContextRc, main_loop::MainLoopRc};

use super::graph::{Graph, Node, PLAYBACK_CLASS, Port};
use super::{Assembler, Feed, Support};
use crate::MediaError;
use crate::audio::AudioSettings;

/// A node id PipeWire has not given yet (`SPA_ID_INVALID`).
const NO_NODE: u32 = u32::MAX;

type Reply = mpsc::Sender<Result<pipewire::channel::Sender<()>, &'static str>>;

/// Whether the session's PipeWire answers. Found once per run.
pub fn support() -> Support {
    static FOUND: OnceLock<Support> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            pipewire::init();
            let reachable = MainLoopRc::new(None).ok().and_then(|main_loop| {
                let context = ContextRc::new(&main_loop, None).ok()?;
                context.connect_rc(None).ok().map(|_| ())
            });
            match reachable {
                Some(()) => Support::Ready,
                None => Support::Unavailable("Needs PipeWire"),
            }
        })
        .clone()
}

/// Every app's sound on this computer, except Veil's own.
pub struct PipeWireSound {
    feed: Arc<Feed>,
    frames: Assembler,
    quit: pipewire::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireSound {
    pub fn open() -> Result<Self, MediaError> {
        let feed = Arc::new(Feed::default());
        let (reply, answer) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("veil-share-sound".into())
            .spawn({
                let feed = feed.clone();
                move || {
                    let reply_on_error = reply.clone();
                    if let Err(why) = pump(&feed, &reply) {
                        let _ = reply_on_error.send(Err(why));
                        feed.end(why);
                    }
                }
            })
            .map_err(|_| MediaError::Capture("thread"))?;
        let quit = answer
            .recv()
            .map_err(|_| MediaError::Capture("pipewire"))?
            .map_err(MediaError::Capture)?;
        Ok(Self {
            feed,
            frames: Assembler::new(&AudioSettings::share_sound()),
            quit,
            thread: Some(thread),
        })
    }

    /// Waits for the next frame of [`AudioSettings::share_sound`], at most until it is due.
    pub fn read(&mut self) -> Result<&[f32], MediaError> {
        self.feed.read(&mut self.frames)
    }
}

impl Drop for PipeWireSound {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The capture format: 48 kHz float stereo, left and right.
fn format() -> Result<Vec<u8>, &'static str> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(48_000);
    info.set_channels(2);
    let mut position = [0; pipewire::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let object = pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|_| "pipewire format")
}

/// Runs PipeWire until the quit message. Sends the quit sender once capture is set up.
fn pump(feed: &Arc<Feed>, reply: &Reply) -> Result<(), &'static str> {
    pipewire::init();
    let main_loop = MainLoopRc::new(None).map_err(|_| "pipewire loop")?;
    let context = ContextRc::new(&main_loop, None).map_err(|_| "pipewire context")?;
    let core = context.connect_rc(None).map_err(|_| "pipewire connect")?;
    let registry = core.get_registry_rc().map_err(|_| "pipewire registry")?;
    let stream = StreamRc::new(
        core.clone(),
        "Veil computer sound",
        pipewire::properties::properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
            "node.autoconnect" => "false",
        },
    )
    .map_err(|_| "pipewire stream")?;

    let graph = Rc::new(RefCell::new(Graph::default()));
    let links: Rc<RefCell<Vec<Link>>> = Rc::default();
    let relink = Rc::new({
        let (graph, links, stream, core) = (graph.clone(), links.clone(), stream.clone(), core);
        move || {
            let capture = stream.node_id();
            if capture == NO_NODE {
                return;
            }
            let missing = graph.borrow_mut().missing(capture, std::process::id());
            for (output, input) in missing {
                let properties = pipewire::properties::properties! {
                    "link.output.port" => output.to_string(),
                    "link.input.port" => input.to_string(),
                    "object.linger" => "false",
                };
                if let Ok(link) = core.create_object::<Link>("link-factory", &properties) {
                    links.borrow_mut().push(link);
                }
            }
        }
    });

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (graph, relink) = (graph.clone(), relink.clone());
            move |global| {
                let get = |key: &str| global.props.and_then(|props| props.get(key));
                match global.type_ {
                    ObjectType::Node => graph.borrow_mut().add_node(
                        global.id,
                        Node {
                            playback: get("media.class") == Some(PLAYBACK_CLASS),
                            process: get("application.process.id").and_then(|p| p.parse().ok()),
                        },
                    ),
                    ObjectType::Port => {
                        let Some(node) = get("node.id").and_then(|n| n.parse().ok()) else {
                            return;
                        };
                        graph.borrow_mut().add_port(
                            global.id,
                            Port {
                                node,
                                output: get("port.direction") == Some("out"),
                                channel: get("audio.channel").unwrap_or_default().to_string(),
                            },
                        );
                    }
                    _ => return,
                }
                (*relink)();
            }
        })
        .global_remove({
            let graph = graph.clone();
            move |id| graph.borrow_mut().remove(id)
        })
        .register();

    let _stream_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let (feed, relink) = (feed.clone(), relink.clone());
            move |_, _, _, now| match now {
                StreamState::Error(_) | StreamState::Unconnected => {
                    feed.end("computer sound stopped");
                }
                StreamState::Paused | StreamState::Streaming => (*relink)(),
                StreamState::Connecting => {}
            }
        })
        .process({
            let feed = feed.clone();
            move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
                let Some(bytes) = data.data().and_then(|b| b.get(offset..offset + size)) else {
                    return;
                };
                let sound: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                feed.push(&sound);
            }
        })
        .register()
        .map_err(|_| "pipewire listener")?;

    let bytes = format()?;
    let mut params = [Pod::from_bytes(&bytes).ok_or("pipewire format")?];
    stream
        .connect(
            Direction::Input,
            None,
            StreamFlags::MAP_BUFFERS,
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
    links.borrow_mut().clear();
    Ok(())
}
