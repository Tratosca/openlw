//! PipeWire loop: two streams (sink/source) in a dedicated thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use lw_sys::shm::{Consumer, Dir, Producer, Region};
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

struct Control {
    stop: AtomicBool,
    /// Wakes the main loop of the current connection attempt.
    current: Mutex<Option<pw::channel::Sender<()>>>,
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
            let _ = q.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Publish the nodes on `region` (TO_NET producer, FROM_NET consumer) from a dedicated thread.
/// Connection failures are retried every 5 s and reported through `report`; only a thread
/// creation failure is an error here.
pub fn start(region: Arc<Region>, cfg: BridgeConfig, report: Report) -> Result<Bridge, Error> {
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        current: Mutex::new(None),
    });
    let ctl = control.clone();
    let thread = std::thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || supervise(&region, &cfg, &ctl, report))
        .map_err(|e| Error(format!("thread PipeWire : {e}")))?;
    Ok(Bridge {
        control,
        thread: Some(thread),
    })
}

fn supervise(region: &Region, cfg: &BridgeConfig, control: &Control, report: Report) {
    let mut last_error = String::new();
    while !control.stop.load(Ordering::Acquire) {
        let (tx, rx) = pw::channel::channel::<()>();
        *control
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tx);
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let result = run(region, cfg, rx, || {
            last_error.clear();
            report(false, "nœuds PipeWire publiés (OpenLW In, OpenLW Out)");
        });
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let message = match result {
            Ok(()) => "connexion à PipeWire perdue".to_string(),
            Err(e) => e,
        };
        // Same failure repeated every 5 s: report it once.
        if message != last_error {
            report(
                true,
                &format!("{message} ; nouvelle tentative toutes les 5 s"),
            );
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
        .map_err(|e| format!("format PipeWire : {e:?}"))
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
    quit: pw::channel::Receiver<()>,
    on_ready: impl FnOnce(),
) -> Result<(), String> {
    let pwerr = |what: &str, e: pw::Error| format!("PipeWire, {what} : {e}");
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| pwerr("boucle", e))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| pwerr("contexte", e))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| pwerr("connexion au serveur (session PipeWire absente ?)", e))?;
    let _quit = quit.attach(mainloop.loop_(), {
        let ml = mainloop.clone();
        move |()| ml.quit()
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
    let producer = region.producer(Dir::ToNet).map_err(|e| e.to_string())?;
    let consumer = region.consumer(Dir::FromNet).map_err(|e| e.to_string())?;
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;

    // Sink: applications play, audio goes to network.
    let ch_out = producer.channels();
    let sink = pw::stream::StreamBox::new(
        &core,
        "OpenLW Out",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => "openlw_out",
            *pw::keys::NODE_DESCRIPTION => cfg.sink_description.as_str(),
            *pw::keys::AUDIO_CHANNELS => ch_out.to_string(),
            "audio.position" => channel_positions(ch_out),
        },
    )
    .map_err(|e| pwerr("puits", e))?;
    let sink_state = SinkState {
        producer,
        scratch: vec![0.0; MAX_FRAMES * ch_out as usize],
    };
    let _sink_listener = sink
        .add_local_listener_with_user_data(sink_state)
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else {
                return;
            };
            let (offset, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
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
        .map_err(|e| pwerr("puits", e))?;
    let sink_pod = format_pod(ch_out)?;
    let mut sink_params = [Pod::from_bytes(&sink_pod).ok_or("format du puits invalide")?];
    sink.connect(spa::utils::Direction::Input, None, flags, &mut sink_params)
        .map_err(|e| pwerr("puits", e))?;

    // Source: applications record network audio.
    let ch_in = consumer.channels();
    let source = pw::stream::StreamBox::new(
        &core,
        "OpenLW In",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::NODE_NAME => "openlw_in",
            *pw::keys::NODE_DESCRIPTION => cfg.source_description.as_str(),
            *pw::keys::AUDIO_CHANNELS => ch_in.to_string(),
            "audio.position" => channel_positions(ch_in),
        },
    )
    .map_err(|e| pwerr("source", e))?;
    let source_state = SourceState {
        consumer,
        margin: cfg.input_margin.clamp(64, 2048),
        primed: false,
        scratch: vec![0.0; MAX_FRAMES * ch_in as usize],
    };
    let _source_listener = source
        .add_local_listener_with_user_data(source_state)
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
        .map_err(|e| pwerr("source", e))?;
    let source_pod = format_pod(ch_in)?;
    let mut source_params = [Pod::from_bytes(&source_pod).ok_or("format de la source invalide")?];
    source
        .connect(
            spa::utils::Direction::Output,
            None,
            flags,
            &mut source_params,
        )
        .map_err(|e| pwerr("source", e))?;

    on_ready();
    mainloop.run();
    Ok(())
}
