//! Internal audio loopback: a mock audio client acts as the HAL plugin or Windows driver.
//! It obtains the region through the control channel (`attach`: XPC on macOS, named pipe on
//! Windows; region stays in-process on Linux), writes application audio, reads internal
//! loopback and shared clock; the daemon exposes peaks and counters through `status`.

#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lw_daemon::control::Shared;
use lw_daemon::device::{self, DeviceConfig};
use lw_daemon::iface::Iface;
use lw_daemon::Stop;
use lw_sys::shm::{Dir, Region};
use serde_json::Value;

/// Client side: control channel (kept open) and region obtained through `attach`.
struct FakeClient {
    _server: lw_sys::ctl::Server,
    call: Box<dyn Fn(&str) -> String>,
    attach_reply: Value,
    region: Arc<Region>,
}

#[cfg(target_os = "macos")]
fn attach(shared: &Shared, dev: &device::Device) -> FakeClient {
    let server = shared.serve_anonymous_xpc().unwrap();
    server.set_region(&dev.region);
    let client = lw_sys::xpc::Client::from_endpoint(&server.xpc().unwrap().endpoint()).unwrap();
    let (reply, obj) = client.call_with_shmem(r#"{"cmd":"attach"}"#).unwrap();
    let region = Region::map(obj.expect("region attached to the attach reply")).unwrap();
    FakeClient {
        _server: server,
        call: Box::new(move |r| client.call(r).unwrap()),
        attach_reply: serde_json::from_str(&reply).unwrap(),
        region: Arc::new(region),
    }
}

#[cfg(windows)]
fn attach(shared: &Shared, dev: &device::Device) -> FakeClient {
    let ep = lw_sys::ctl::Endpoint::Pipe(format!(
        "fr.francois-brille.openlw.device-test.{}",
        std::process::id()
    ));
    let server = shared.serve(&ep).unwrap();
    server.set_region(&dev.region);
    let client = lw_sys::ctl::Client::connect(&ep).unwrap();
    let (reply, obj) = client.call_with_region(r#"{"cmd":"attach"}"#).unwrap();
    let region = Region::map(obj.expect("section attached to the attach reply")).unwrap();
    FakeClient {
        _server: server,
        call: Box::new(move |r| client.call(r).unwrap()),
        attach_reply: serde_json::from_str(&reply).unwrap(),
        region: Arc::new(region),
    }
}

/// Linux: daemon serves PipeWire nodes itself; region remains in-process.
#[cfg(target_os = "linux")]
fn attach(shared: &Shared, dev: &device::Device) -> FakeClient {
    let ep = lw_sys::ctl::Endpoint::Socket(
        std::env::temp_dir().join(format!("openlw-device-{}.sock", std::process::id())),
    );
    let server = shared.serve(&ep).unwrap();
    server.set_region(&dev.region);
    let client = lw_sys::ctl::Client::connect(&ep).unwrap();
    let reply = client.call(r#"{"cmd":"attach"}"#).unwrap();
    FakeClient {
        _server: server,
        call: Box::new(move |r| client.call(r).unwrap()),
        attach_reply: serde_json::from_str(&reply).unwrap(),
        region: dev.region.clone(),
    }
}

#[test]
fn fake_plugin_loopback_roundtrip() {
    let stop = Stop::new();
    let cfg = DeviceConfig {
        channels_to_net: 2,
        channels_from_net: 2,
        ring_frames: 4096,
        loopback: true,
    };
    let dev = device::start(&cfg, &stop).unwrap();
    let lo = Iface {
        name: "lo0".into(),
        friendly: "lo0".into(),
        index: 1,
        ipv4: Ipv4Addr::LOCALHOST,
        loopback: true,
    };
    let shared = Shared::new(Some(&lo), 0);
    shared.set_device(dev.status.clone());

    // --- audio client side ---
    let client = attach(&shared, &dev);
    let reply = &client.attach_reply;
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["device"]["channels_to_net"], 2);
    let region = &client.region;
    let mut to_net = region.producer(Dir::ToNet).unwrap();
    let mut from_net = region.consumer(Dir::FromNet).unwrap();

    // Clock: position advances at 48 kHz relative to the host clock.
    std::thread::sleep(Duration::from_millis(30));
    let (h1, s1, rate) = region.clock().expect("clock published");
    std::thread::sleep(Duration::from_millis(200));
    let (h2, s2, _) = region.clock().unwrap();
    assert_eq!(rate, 1.0);
    let dt_ns = lw_sys::rt::host_time_ns_of(h2 - h1);
    let expected = dt_ns as f64 * 48_000.0 / 1e9;
    assert!(
        ((s2 - s1) as f64 - expected).abs() < 48.0,
        "position {} versus {expected:.0} expected",
        s2 - s1
    );

    // Audio: 1 kHz, −6 dBFS, both channels, real-time pacing (48 frames/ms) for 0.5 s.
    let amp = 10f32.powf(-6.0 / 20.0);
    let total = 24_000usize;
    let mut sent = Vec::with_capacity(total);
    let mut received = Vec::with_capacity(total);
    let mut back = vec![0f32; 2 * 4096];
    let start = Instant::now();
    let mut i = 0usize;
    while received.len() < total && start.elapsed() < Duration::from_secs(3) {
        if i < total {
            let block: Vec<f32> = (i..i + 48)
                .flat_map(|n| {
                    let s = amp * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
                    [s, -s]
                })
                .collect();
            assert_eq!(to_net.write(&block).unwrap(), 48);
            sent.extend(block.chunks_exact(2).map(|f| f[0]));
            i += 48;
        }
        let n = from_net.readable() as usize;
        if n > 0 {
            from_net.read(&mut back[..2 * n]).unwrap();
            for f in back[..2 * n].chunks_exact(2) {
                assert_eq!(f[1], -f[0], "channels intact");
                received.push(f[0]);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        received.len(),
        total,
        "every frame comes back through the internal loopback"
    );
    assert_eq!(received, sent, "identical samples, in order");

    // State through the control channel: no overrun.
    std::thread::sleep(Duration::from_millis(150));
    let status: Value = serde_json::from_str(&(client.call)(r#"{"cmd":"status"}"#)).unwrap();
    let d = &status["status"]["device"];
    assert_eq!(d["to_net_overruns"], 0);
    assert_eq!(d["from_net_overruns"], 0);
    assert!(d["to_net_frames"].as_u64().unwrap() >= total as u64);

    stop.request();
    dev.thread.join().unwrap();
}

#[test]
fn meters_follow_signal_level() {
    let stop = Stop::new();
    let cfg = DeviceConfig {
        channels_to_net: 4,
        channels_from_net: 2,
        ring_frames: 4096,
        loopback: false,
    };
    let dev = device::start(&cfg, &stop).unwrap();
    let mut to_net = dev.region.producer(Dir::ToNet).unwrap();
    // 200 ms: channel 0 at −12 dBFS, channel 2 at −3 dBFS, channels 1 and 3 silent.
    let (a, b) = (10f32.powf(-12.0 / 20.0), 10f32.powf(-3.0 / 20.0));
    for _ in 0..200 {
        let block: Vec<f32> = (0..48).flat_map(|_| [a, 0.0, b, 0.0]).collect();
        to_net.write(&block).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(50));
    let s = dev.snapshot();
    assert!(
        (s.to_net_peak_dbfs[0] + 12.0).abs() < 0.1,
        "{:?}",
        s.to_net_peak_dbfs
    );
    assert!(
        (s.to_net_peak_dbfs[2] + 3.0).abs() < 0.1,
        "{:?}",
        s.to_net_peak_dbfs
    );
    assert!(s.to_net_peak_dbfs[1].is_infinite() && s.to_net_peak_dbfs[3].is_infinite());
    stop.request();
    dev.thread.join().unwrap();
}

/// Host writes 4096-frame blocks (≈ 85 ms): patched output must reach
/// the transmitter smoothly (240 frames every 5 ms, jitter buffer like a Standard stream).
#[test]
fn large_host_blocks_reach_the_stream_smoothly() {
    use lw_daemon::bus::{bus, JitterReader};
    use lw_daemon::device::{OutRoute, Routes};
    let stop = Stop::new();
    let cfg = DeviceConfig {
        channels_to_net: 2,
        channels_from_net: 2,
        ring_frames: 8192,
        loopback: false,
    };
    let dev = device::start(&cfg, &stop).unwrap();
    let (writer, reader) = bus(2, lw_daemon::patch::BUS_FRAMES);
    dev.routes_handle().set(Routes {
        outputs: vec![OutRoute {
            label: "test".into(),
            device_channels: vec![0, 1],
            writer,
        }],
        inputs: Vec::new(),
    });
    let mut jitter = JitterReader::new(reader, 2 * 240 + 512, 2 * 240 + 512 + 2048);
    let mut to_net = dev.region.producer(Dir::ToNet).unwrap();
    let block: Vec<f32> = vec![0.5; 4096 * 2];
    let t0 = Instant::now();
    let mut blocks = 0u32;
    let mut packets = 0u32;
    let mut out = [0f32; 240 * 2];
    let mut primed_at = None;
    let mut silent_after_prime = 0u32;
    while t0.elapsed() < Duration::from_millis(2500) {
        // Host block due: write at once.
        if t0.elapsed() >= Duration::from_micros(85_333 * u64::from(blocks)) {
            to_net.write(&block).unwrap();
            blocks += 1;
        }
        // Packet due (5 ms).
        if t0.elapsed() >= Duration::from_millis(5 * u64::from(packets)) {
            jitter.pull(&mut out);
            packets += 1;
            if jitter.primed() && primed_at.is_none() {
                primed_at = Some(packets);
            }
            if primed_at.is_some() && out.iter().all(|&v| v == 0.0) {
                silent_after_prime += 1;
            }
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    let c = jitter.counters();
    assert_eq!(c.slips, 0, "no frame discarded: {c:?}");
    assert_eq!(
        silent_after_prime, 0,
        "no silent packet after priming: {c:?}"
    );
    stop.request();
    dev.thread.join().unwrap();
}
