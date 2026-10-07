//! Headphone preview without the service: the app joins the source multicast group on the
//! Livewire interface, decodes RTP L24/L16 and plays the first two channels through a PipeWire
//! playback stream (default output). Same behavior as the macOS and Windows apps: 30 ms buffer
//! before playback, excess above 200 ms discarded (sender drift).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::pod::Pod;
use socket2::{Domain, Protocol, Socket, Type};

const RATE: u32 = 48_000;
const PORT: u16 = lw_proto::channel::AUDIO_PORT;
/// Stereo samples (interleaved) buffered before playback, and maximum kept.
const TARGET: usize = 2 * RATE as usize * 30 / 1000;
const HIGH: usize = 2 * RATE as usize * 200 / 1000;

#[derive(Default)]
struct Ring {
    samples: VecDeque<f32>,
    playing: bool,
    /// Peak (linear) since the last `take_peak`.
    peak: f32,
}

struct Running {
    stop: Arc<AtomicBool>,
    quit: pw::channel::Sender<()>,
}

#[derive(Default)]
pub struct Listener {
    ring: Arc<Mutex<Ring>>,
    running: Option<Running>,
}

impl Listener {
    /// Peak (dBFS) since the last call, `None` for silence.
    pub fn take_peak(&self) -> Option<f64> {
        let mut r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let p = std::mem::take(&mut r.peak);
        (p > 0.0).then(|| 20.0 * f64::from(p).log10())
    }

    /// Starts the preview of `group` received on the interface with address `iface`.
    /// `channels`: stream channels (two, or eight for surround); only the first two are played.
    pub fn start(
        &mut self,
        group: Ipv4Addr,
        iface: Ipv4Addr,
        channels: usize,
        bits: u32,
    ) -> Result<(), String> {
        self.stop();
        let sock = open(group, iface)?;
        *self.ring.lock().unwrap_or_else(PoisonError::into_inner) = Ring::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let ring = self.ring.clone();
        std::thread::Builder::new()
            .name("preview PipeWire".into())
            .spawn(move || {
                let r = play(&ring, quit_rx, &ready_tx);
                if let Err(e) = r {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| format!("Cannot listen to the source: {e}."))?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Cannot listen to the source: {e}.")),
            Err(_) => {
                let _ = quit.send(());
                return Err("Cannot listen to the source: PipeWire is not responding.".into());
            }
        }
        let ring = self.ring.clone();
        let st = stop.clone();
        let width = if bits == 16 { 2 } else { 3 };
        std::thread::Builder::new()
            .name("preview network".into())
            .spawn(move || receive(&sock, channels.max(1), width, &ring, &st))
            .map_err(|e| format!("Cannot listen to the source: {e}."))?;
        self.running = Some(Running { stop, quit });
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop.store(true, Ordering::Release);
            let _ = r.quit.send(());
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Shared port (OpenLW service, other Livewire software). Bound to the group address: the
/// kernel delivers only this group's datagrams to the socket.
fn open(group: Ipv4Addr, iface: Ipv4Addr) -> Result<UdpSocket, String> {
    let fail = |step: &str, e: std::io::Error| {
        format!("Cannot listen to the source ({step}: {e}). Check the selected Livewire interface.")
    };
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| fail("socket", e))?;
    s.set_reuse_address(true).map_err(|e| fail("socket", e))?;
    s.set_reuse_port(true).map_err(|e| fail("socket", e))?;
    s.bind(&SocketAddrV4::new(group, PORT).into())
        .map_err(|e| fail("bind", e))?;
    s.join_multicast_v4(&group, &iface)
        .map_err(|e| fail("group join", e))?;
    s.set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| fail("socket", e))?;
    Ok(s.into())
}

fn receive(sock: &UdpSocket, channels: usize, width: usize, ring: &Mutex<Ring>, stop: &AtomicBool) {
    let mut packet = [0u8; 2048];
    let mut frames: Vec<f32> = Vec::with_capacity(2 * 512);
    let stride = channels * width;
    while !stop.load(Ordering::Acquire) {
        let Ok(n) = sock.recv(&mut packet) else {
            continue; // timeout: check for stop
        };
        let Some(Ok(p)) = packet.get(..n).map(lw_proto::rtp::Packet::parse) else {
            continue;
        };
        frames.clear();
        let mut peak = 0f32;
        for frame in p.payload.chunks_exact(stride).take(512) {
            for c in 0..2 {
                let i = c.min(channels - 1) * width;
                let v = match frame.get(i..i + width) {
                    Some(&[a, b, c]) => i32::from_be_bytes([a, b, c, 0]) as f32 / 2_147_483_648.0,
                    Some(&[a, b]) => f32::from(i16::from_be_bytes([a, b])) / 32_768.0,
                    _ => 0.0,
                };
                peak = peak.max(v.abs());
                frames.push(v);
            }
        }
        let mut r = ring.lock().unwrap_or_else(PoisonError::into_inner);
        if r.samples.len() > HIGH {
            r.samples.clear();
            r.playing = false;
        }
        r.samples.extend(frames.iter());
        r.peak = r.peak.max(peak);
    }
}

/// PipeWire playback stream; returns when `quit` fires or the server goes away.
fn play(
    ring: &Arc<Mutex<Ring>>,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let pwerr = |what: &str, e: pw::Error| format!("PipeWire, {what}: {e}");
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| pwerr("loop", e))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| pwerr("context", e))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| pwerr("connection (no audio session?)", e))?;
    // Quit request or server error: no further loop run.
    let ended = Rc::new(Cell::new(false));
    let _quit = quit.attach(mainloop.loop_(), {
        let (ml, ended) = (mainloop.clone(), ended.clone());
        move |()| {
            ended.set(true);
            ml.quit();
        }
    });
    // Output discovery: two server roundtrips (globals, then the bound default metadata).
    let sinks: Rc<RefCell<Vec<String>>> = Rc::default();
    let default_sink: Rc<RefCell<Option<String>>> = Rc::default();
    let metadata: Rc<RefCell<Vec<(pw::metadata::Metadata, pw::metadata::MetadataListener)>>> =
        Rc::default();
    let registry = core.get_registry_rc().map_err(|e| pwerr("registry", e))?;
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (registry, sinks) = (registry.clone(), sinks.clone());
            let (metadata, default_sink) = (metadata.clone(), default_sink.clone());
            move |g| {
                let props = g.props.as_ref();
                let prop = |k: &str| props.and_then(|p| p.get(k));
                match g.type_ {
                    pw::types::ObjectType::Node if prop("media.class") == Some("Audio/Sink") => {
                        if let Some(name) = prop("node.name") {
                            sinks.borrow_mut().push(name.to_string());
                        }
                    }
                    pw::types::ObjectType::Metadata if prop("metadata.name") == Some("default") => {
                        let Ok(m) = registry.bind::<pw::metadata::Metadata, _>(g) else {
                            return;
                        };
                        let default_sink = default_sink.clone();
                        let listener = m
                            .add_listener_local()
                            .property(move |_subject, key, _type, value| {
                                if key == Some("default.audio.sink") {
                                    *default_sink.borrow_mut() = value.and_then(metadata_name);
                                }
                                0
                            })
                            .register();
                        metadata.borrow_mut().push((m, listener));
                    }
                    _ => {}
                }
            }
        })
        .register();
    let pending = Rc::new(Cell::new(None));
    let roundtrips = Rc::new(Cell::new(0));
    let _core_listener = core
        .add_listener_local()
        .done({
            let (ml, core) = (mainloop.clone(), core.clone());
            let (pending, roundtrips) = (pending.clone(), roundtrips.clone());
            move |id, seq| {
                if id != pw::core::PW_ID_CORE || pending.get() != Some(seq) {
                    return;
                }
                roundtrips.set(roundtrips.get() + 1);
                match core.sync(0) {
                    Ok(next) if roundtrips.get() < 2 => pending.set(Some(next)),
                    _ => ml.quit(),
                }
            }
        })
        .error({
            let (ml, ended) = (mainloop.clone(), ended.clone());
            move |id, _seq, _res, _message| {
                if id == pw::core::PW_ID_CORE {
                    ended.set(true);
                    ml.quit();
                }
            }
        })
        .register();
    pending.set(Some(core.sync(0).map_err(|e| pwerr("synchronization", e))?));
    mainloop.run();
    if ended.get() {
        return Ok(());
    }
    let target = preview_target(&sinks.borrow(), default_sink.borrow().as_deref())?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "OpenLW Preview",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => "openlw_preview",
            *pw::keys::APP_NAME => "OpenLW",
            // Never OpenLW Out: the preview would go back to the network.
            "target.object" => target.as_str(),
            "node.dont-reconnect" => "true",
        },
    )
    .map_err(|e| pwerr("flux", e))?;
    let _listener = stream
        .add_local_listener_with_user_data(ring.clone())
        .process(|stream, ring| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else {
                return;
            };
            let stride = 8; // stereo f32
            let mut frames = 0;
            if let Some(bytes) = d.data() {
                let capacity = bytes.len() / stride;
                frames = if requested > 0 {
                    requested.min(capacity)
                } else {
                    capacity
                };
                let mut r = ring.lock().unwrap_or_else(PoisonError::into_inner);
                if !r.playing && r.samples.len() >= TARGET {
                    r.playing = true;
                }
                for b in bytes.as_chunks_mut::<4>().0.iter_mut().take(frames * 2) {
                    let v = if r.playing {
                        r.samples.pop_front().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    *b = v.to_le_bytes();
                }
                // Underrun: wait for the buffer to fill again.
                if r.samples.is_empty() {
                    r.playing = false;
                }
            }
            let chunk = d.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (frames * stride) as _;
        })
        .register()
        .map_err(|e| pwerr("flux", e))?;
    let pod = format_pod()?;
    let mut params = [Pod::from_bytes(&pod).ok_or("invalid format")?];
    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| pwerr("flux", e))?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// Sink node name of the daemon (lw-pw).
const OPENLW_OUT: &str = "openlw_out";

/// `{"name":"…"}` value of the `default.audio.sink` metadata key.
fn metadata_name(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()?
        .get("name")?
        .as_str()
        .map(String::from)
}

/// Preview output: the default output, or the first other one when the default is OpenLW Out
/// (or unknown).
fn preview_target(sinks: &[String], default: Option<&str>) -> Result<String, String> {
    if let Some(d) = default.filter(|d| *d != OPENLW_OUT && sinks.iter().any(|s| s == d)) {
        return Ok(d.to_string());
    }
    sinks
        .iter()
        .find(|s| *s != OPENLW_OUT)
        .cloned()
        .ok_or_else(|| "no audio output on this computer other than OpenLW Out".to_string())
}

/// Interleaved 32-bit float, 48 kHz, stereo.
fn format_pod() -> Result<Vec<u8>, String> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(2);
    let mut position = [0u32; spa::param::audio::MAX_CHANNELS];
    if let Some(p) = position.get_mut(..2) {
        p.copy_from_slice(&[
            spa::sys::SPA_AUDIO_CHANNEL_FL,
            spa::sys::SPA_AUDIO_CHANNEL_FR,
        ]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_never_targets_openlw_out() {
        let sinks = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let both = sinks(&["openlw_out", "speakers"]);
        assert_eq!(
            preview_target(&both, Some("speakers")),
            Ok("speakers".into())
        );
        assert_eq!(
            preview_target(&both, Some("openlw_out")),
            Ok("speakers".into())
        );
        assert_eq!(preview_target(&both, None), Ok("speakers".into()));
        assert_eq!(preview_target(&both, Some("gone")), Ok("speakers".into()));
        assert!(preview_target(&sinks(&["openlw_out"]), Some("openlw_out")).is_err());
        assert!(preview_target(&[], None).is_err());
        assert_eq!(
            metadata_name(r#"{"name":"speakers"}"#),
            Some("speakers".into())
        );
        assert_eq!(metadata_name("{}"), None);
    }
}
