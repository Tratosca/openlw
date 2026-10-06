//! Boucle audio interne : un « faux plugin » joue le rôle du plugin HAL.
//! Il obtient la région par XPC (`attach`), écrit l'audio des applications, relit le retour
//! en boucle interne, lit l'horloge partagée ; le daemon expose crêtes et compteurs par `status`.

#![cfg(target_os = "macos")]
#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use lw_daemon::control::Shared;
use lw_daemon::device::{self, DeviceConfig};
use lw_daemon::iface::Iface;
use lw_daemon::Stop;
use lw_sys::shm::{Dir, Region};
use serde_json::Value;

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
    let server = shared.serve(None).unwrap();
    server.set_shmem(&dev.region);

    // --- côté « plugin » ---
    let client = lw_sys::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
    let (reply, obj) = client.call_with_shmem(r#"{"cmd":"attach"}"#).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["device"]["channels_to_net"], 2);
    let region = Region::map(obj.expect("région jointe à la réponse attach")).unwrap();
    let mut to_net = region.producer(Dir::ToNet).unwrap();
    let mut from_net = region.consumer(Dir::FromNet).unwrap();

    // Horloge : la position avance à 48 kHz par rapport à l'horloge hôte.
    std::thread::sleep(Duration::from_millis(30));
    let (h1, s1, rate) = region.clock().expect("horloge publiée");
    std::thread::sleep(Duration::from_millis(200));
    let (h2, s2, _) = region.clock().unwrap();
    assert_eq!(rate, 1.0);
    let dt_ns = lw_sys::rt::host_time_ns_of(h2 - h1);
    let expected = dt_ns as f64 * 48_000.0 / 1e9;
    assert!(
        ((s2 - s1) as f64 - expected).abs() < 48.0,
        "position {} contre {expected:.0} attendus",
        s2 - s1
    );

    // Audio : 1 kHz, −6 dBFS, sur les deux canaux, au rythme réel (48 trames par ms) pendant 0,5 s.
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
                assert_eq!(f[1], -f[0], "canaux intacts");
                received.push(f[0]);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        received.len(),
        total,
        "toutes les trames reviennent par la boucle interne"
    );
    assert_eq!(received, sent, "échantillons identiques, dans l'ordre");

    // État vu par XPC : crêtes ≈ −6 dBFS, aucun débordement.
    std::thread::sleep(Duration::from_millis(150));
    let status: Value = serde_json::from_str(&client.call(r#"{"cmd":"status"}"#).unwrap()).unwrap();
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
    // 200 ms : canal 0 à −12 dBFS, canal 2 à −3 dBFS, canaux 1 et 3 muets.
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

/// L'hôte écrit par blocs de 4096 trames (≈ 85 ms) : la sortie patchée doit arriver lissée à
/// l'émetteur (240 trames toutes les 5 ms, tampon de gigue comme un flux Standard).
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
        // Bloc de l'hôte dû : écrit d'un coup.
        if t0.elapsed() >= Duration::from_micros(85_333 * u64::from(blocks)) {
            to_net.write(&block).unwrap();
            blocks += 1;
        }
        // Paquet dû (5 ms).
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
    assert_eq!(c.slips, 0, "aucune trame jetée : {c:?}");
    assert_eq!(
        silent_after_prime, 0,
        "aucun paquet silencieux après l'amorçage : {c:?}"
    );
    stop.request();
    dev.thread.join().unwrap();
}
