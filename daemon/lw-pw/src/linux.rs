//! PipeWire loop: one stream per region ring (sinks and sources) in a dedicated thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use lw_sys::shm::{Consumer, Producer, Region};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::pod::Pod;

use crate::{channel_positions, plan_input, BridgeConfig, Error, InputPlan};

const RATE: u32 = 48_000;
/// Maximum frames per graph cycle (PipeWire maximum quantum: 8192).
const MAX_FRAMES: usize = 8192;

/// Called with `(is_error, message)` for connection events, from the PipeWire thread.
pub type Report = fn(bool, &str);

/// Delay between connection attempts while PipeWire is unreachable.
const RETRY: Duration = Duration::from_secs(5);

/// Message to the PipeWire main loop.
enum Msg {
    Quit,
    /// New node descriptions, in node order.
    Rename(Vec<String>),
}

struct Control {
    stop: AtomicBool,
    /// Nodes currently published (connected to the PipeWire server).
    published: AtomicBool,
    /// Wakes the main loop of the current connection attempt.
    current: Mutex<Option<pw::channel::Sender<Msg>>>,
    /// Current node descriptions (renames survive reconnections).
    descriptions: Mutex<Vec<String>>,
}

/// PipeWire nodes, published as long as this value lives; reconnects automatically when the
/// PipeWire server is missing at startup or restarts.
pub struct Bridge {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        if let Some(q) = self
            .control
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = q.send(Msg::Quit);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Bridge {
    /// True while the nodes are published; false while PipeWire is unreachable.
    pub fn published(&self) -> bool {
        self.control.published.load(Ordering::Acquire)
    }

    /// Change the displayed node names (`node.description`, in node order) without
    /// recreating the nodes: applications keep their streams.
    pub fn rename(&self, descriptions: Vec<String>) {
        *self
            .control
            .descriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = descriptions.clone();
        if let Some(q) = self
            .control
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = q.send(Msg::Rename(descriptions));
        }
    }
}

/// Publish the nodes on `region` (TO_NET ring producers, FROM_NET ring consumers) from a
/// dedicated thread.
/// Connection failures are retried every 5 s and reported through `report`; only a thread
/// creation failure is an error here.
pub fn start(region: Arc<Region>, cfg: BridgeConfig, report: Report) -> Result<Bridge, Error> {
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        published: AtomicBool::new(false),
        current: Mutex::new(None),
        descriptions: Mutex::new(cfg.nodes.iter().map(|n| n.description.clone()).collect()),
    });
    let ctl = control.clone();
    let thread = std::thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || supervise(&region, &cfg, &ctl, report))
        .map_err(|e| Error(format!("PipeWire thread: {e}")))?;
    Ok(Bridge {
        control,
        thread: Some(thread),
    })
}

fn supervise(region: &Region, cfg: &BridgeConfig, control: &Control, report: Report) {
    let mut last_error = String::new();
    while !control.stop.load(Ordering::Acquire) {
        let (tx, rx) = pw::channel::channel::<Msg>();
        *control
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tx);
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        // Latest names (a rename may have happened while disconnected).
        let mut cfg = cfg.clone();
        let names = control
            .descriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for (n, d) in cfg.nodes.iter_mut().zip(names) {
            n.description = d;
        }
        let cfg = &cfg;
        let result = run(region, cfg, rx, || {
            control.published.store(true, Ordering::Release);
            last_error.clear();
            let names: Vec<&str> = cfg.nodes.iter().map(|n| n.description.as_str()).collect();
            report(
                false,
                &format!("PipeWire nodes published ({})", names.join(", ")),
            );
        });
        control.published.store(false, Ordering::Release);
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let message = match result {
            Ok(()) => "connection to PipeWire lost".to_string(),
            Err(e) => e,
        };
        // Same failure repeated every 5 s: report it once.
        if message != last_error {
            report(true, &format!("{message}; retrying every 5 s"));
            last_error = message;
        }
        let until = std::time::Instant::now() + RETRY;
        while std::time::Instant::now() < until && !control.stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Format parameter: interleaved 32-bit float, 48 kHz, `channels` channels.
fn format_pod(channels: u32) -> Result<Vec<u8>, String> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(channels);
    let mut position = [0u32; spa::param::audio::MAX_CHANNELS];
    for (i, p) in position.iter_mut().enumerate().take(channels as usize) {
        *p = match (channels, i) {
            (1, _) => spa::sys::SPA_AUDIO_CHANNEL_MONO,
            (2, 0) => spa::sys::SPA_AUDIO_CHANNEL_FL,
            (2, _) => spa::sys::SPA_AUDIO_CHANNEL_FR,
            _ => spa::sys::SPA_AUDIO_CHANNEL_AUX0 + i as u32,
        };
    }
    info.set_position(position);
    let obj = pw::spa::pod::Value::Object(pw::spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_Format,
        id: spa::sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    });
    pw::spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &obj)
        .map(|(c, _)| c.into_inner())
        .map_err(|e| format!("PipeWire format: {e:?}"))
}

struct SinkState {
    producer: Producer,
    scratch: Vec<f32>,
}

struct SourceState {
    consumer: Consumer,
    margin: u32,
    primed: bool,
    scratch: Vec<f32>,
}

/// One connection: returns `Ok` when the main loop ends (stop requested or server lost),
/// `Err` when the connection or node creation fails.
fn run(
    region: &Region,
    cfg: &BridgeConfig,
    quit: pw::channel::Receiver<Msg>,
    on_ready: impl FnOnce(),
) -> Result<(), String> {
    let pwerr = |what: &str, e: pw::Error| format!("PipeWire, {what}: {e}");
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| pwerr("main loop", e))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| pwerr("context", e))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| pwerr("connecting to the server (no PipeWire session?)", e))?;
    // Raw stream handles for live renames; valid while `streams` lives, i.e. for the whole
    // main loop run (the loop stops before this function drops the streams).
    let raw: Rc<RefCell<Vec<*mut pw::sys::pw_stream>>> = Rc::default();
    let _quit = quit.attach(mainloop.loop_(), {
        let (ml, raw) = (mainloop.clone(), raw.clone());
        move |msg| match msg {
            Msg::Quit => ml.quit(),
            Msg::Rename(names) => {
                for (&stream, name) in raw.borrow().iter().zip(names) {
                    let props = properties! { *pw::keys::NODE_DESCRIPTION => name.as_str() };
                    // SAFETY: `stream` is a live stream of this loop (see `raw`); the
                    // dictionary is valid for the call, PipeWire copies its entries.
                    unsafe {
                        pw::sys::pw_stream_update_properties(stream, props.dict().as_raw());
                    }
                }
            }
        }
    });
    // Server gone (restart, end of session): leave the loop, the caller reconnects.
    let _core_listener = core
        .add_listener_local()
        .error({
            let ml = mainloop.clone();
            move |id, _seq, _res, _message| {
                if id == pw::core::PW_ID_CORE {
                    ml.quit();
                }
            }
        })
        .register();
    // Endpoints are taken only once connected, and released when this attempt ends.
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    let mut streams = Vec::new();
    let mut sink_listeners = Vec::new();
    let mut source_listeners = Vec::new();
    for node in &cfg.nodes {
        let what = if node.sink { "sink" } else { "source" };
        let channels = region.channels(node.ring);
        let stream = pw::stream::StreamBox::new(
            &core,
            &node.description,
            properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_CLASS => if node.sink { "Audio/Sink" } else { "Audio/Source" },
                *pw::keys::NODE_NAME => node.name.as_str(),
                *pw::keys::NODE_DESCRIPTION => node.description.as_str(),
                *pw::keys::AUDIO_CHANNELS => channels.to_string(),
                "audio.position" => channel_positions(channels),
            },
        )
        .map_err(|e| pwerr(what, e))?;
        if node.sink {
            // Sink: applications play, audio goes to network.
            let state = SinkState {
                producer: region.producer(node.ring).map_err(|e| e.to_string())?,
                scratch: vec![0.0; MAX_FRAMES * channels as usize],
            };
            sink_listeners.push(
                stream
                    .add_local_listener_with_user_data(state)
                    .process(|stream, st| {
                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };
                        let datas = buffer.datas_mut();
                        let Some(d) = datas.first_mut() else {
                            return;
                        };
                        let (offset, size) =
                            (d.chunk().offset() as usize, d.chunk().size() as usize);
                        let ch = st.producer.channels().max(1) as usize;
                        if let Some(bytes) = d.data() {
                            let bytes = bytes.get(offset..offset + size).unwrap_or(&[]);
                            let n = (bytes.len() / 4 / ch).min(MAX_FRAMES) * ch;
                            if let Some(out) = st.scratch.get_mut(..n) {
                                for (o, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
                                    *o = f32::from_le_bytes(b.try_into().unwrap_or([0; 4]));
                                }
                                let _ = st.producer.write(out);
                            }
                        }
                    })
                    .register()
                    .map_err(|e| pwerr(what, e))?,
            );
        } else {
            // Source: applications record network audio.
            let state = SourceState {
                consumer: region.consumer(node.ring).map_err(|e| e.to_string())?,
                margin: cfg.input_margin.clamp(64, 2048),
                primed: false,
                scratch: vec![0.0; MAX_FRAMES * channels as usize],
            };
            source_listeners.push(
                stream
                    .add_local_listener_with_user_data(state)
                    .process(|stream, st| {
                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };
                        let ch = st.consumer.channels().max(1) as usize;
                        let stride = ch * 4;
                        let requested = buffer.requested() as usize;
                        let datas = buffer.datas_mut();
                        let Some(d) = datas.first_mut() else {
                            return;
                        };
                        let mut written = 0usize;
                        if let Some(bytes) = d.data() {
                            let capacity = bytes.len() / stride;
                            let frames = if requested > 0 { requested } else { capacity }
                                .min(capacity)
                                .min(MAX_FRAMES);
                            let n = frames * ch;
                            if let Some(buf) = st.scratch.get_mut(..n) {
                                match plan_input(
                                    st.consumer.readable(),
                                    frames as u32,
                                    st.margin,
                                    &mut st.primed,
                                ) {
                                    InputPlan::Silence => buf.iter_mut().for_each(|v| *v = 0.0),
                                    InputPlan::Read { skip } => {
                                        if skip > 0 {
                                            st.consumer.skip(skip);
                                        }
                                        let _ = st.consumer.read(buf);
                                    }
                                }
                                for (b, v) in bytes.chunks_exact_mut(4).zip(buf.iter()) {
                                    b.copy_from_slice(&v.to_le_bytes());
                                }
                                written = frames;
                            }
                        }
                        let chunk = d.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = stride as _;
                        *chunk.size_mut() = (written * stride) as _;
                    })
                    .register()
                    .map_err(|e| pwerr(what, e))?,
            );
        }
        let pod = format_pod(channels)?;
        let mut params = [Pod::from_bytes(&pod).ok_or("invalid stream format")?];
        let direction = if node.sink {
            spa::utils::Direction::Input
        } else {
            spa::utils::Direction::Output
        };
        stream
            .connect(direction, None, flags, &mut params)
            .map_err(|e| pwerr(what, e))?;
        raw.borrow_mut().push(stream.as_raw_ptr());
        streams.push(stream);
    }

    on_ready();
    mainloop.run();
    Ok(())
}
