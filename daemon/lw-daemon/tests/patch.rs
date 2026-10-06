//! Patch de bout en bout sur l'interface de bouclage : découverte/patch par la commande de contrôle, rechargement de
//! session, audio réseau → entrées du périphérique et sorties du périphérique → réseau.

#![allow(clippy::indexing_slicing)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use lw_daemon::config::Config;
use lw_daemon::control::Shared;
use lw_daemon::device::{self, DeviceConfig};
use lw_daemon::net::TxOptions;
use lw_daemon::supervisor::Session;
use lw_daemon::{iface, rx, tx, Stop};
use lw_proto::channel::Channel;
use lw_proto::format::StreamFormat;
use lw_sys::ctl::Caller;
use lw_sys::shm::Dir;
use serde_json::Value;

fn db(x: f32) -> f64 {
    20.0 * f64::from(x).log10()
}

#[test]
fn patch_network_to_device_and_back() {
    let lo = iface::list()
        .unwrap()
        .into_iter()
        .find(|i| i.loopback)
        .unwrap();
    let stop = Stop::new();
    let dev = device::start(
        &DeviceConfig {
            channels_to_net: 8,
            channels_from_net: 8,
            ring_frames: 8192,
            loopback: false,
        },
        &stop,
    )
    .unwrap();
    let routes = dev.routes_handle();
    let shared = Shared::new(Some(&lo), 0);
    shared.set_device(dev.status.clone());
    let cfg: Config = serde_json::from_value(serde_json::json!({
        "iface": lo.name, "advertise": false,
        "device": {"channels_to_net": 8, "channels_from_net": 8}
    }))
    .unwrap();
    let (reload_tx, reload_rx) = mpsc::channel();
    shared.set_config(cfg.clone(), None, reload_tx);
    let mut session = Session::start(&cfg, lo.clone(), &shared, Some(&routes), &stop).unwrap();
    let reload = |session: &mut Session| {
        let next = reload_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rechargement demandé");
        let old = std::mem::replace(
            session,
            Session::start(&next, lo.clone(), &shared, Some(&routes), &stop).unwrap(),
        );
        old.stop();
    };

    // Terminal simulé : canal 21, Standard, 1 kHz à −12 dBFS.
    let omnia_stop = Stop::new();
    let omnia = {
        let (lo, s) = (lo.clone(), omnia_stop.clone());
        let mut stream = tx::TxStream::new(Channel::new(21).unwrap(), StreamFormat::Standard);
        stream.tone = tx::Tone::new(1000.0, -12.0);
        std::thread::spawn(move || tx::run(&lo, &stream, TxOptions::default(), &s).unwrap())
    };

    // Appelant non autorisé : refusé.
    let denied: Value = serde_json::from_str(&shared.handle(
        r#"{"cmd":"patch_input","channel":21,"device_channels":[3,4]}"#,
        &Caller {
            may_edit: false,
            ..Caller::trusted("invité")
        },
    ))
    .unwrap();
    assert_eq!(denied["ok"], false);
    assert!(denied["error"]
        .as_str()
        .unwrap()
        .contains(lw_sys::ctl::edit_policy()));

    // Patch : canal 21 → entrées 3-4.
    let r: Value = serde_json::from_str(&shared.handle(
        r#"{"cmd":"patch_input","channel":21,"device_channels":[3,4]}"#,
        &Caller::trusted("test"),
    ))
    .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    reload(&mut session);

    // Faux plugin : lit l'anneau réseau → applications au rythme réel.
    let mut from_net = dev.region.consumer(Dir::FromNet).unwrap();
    let mut peaks = [0f32; 8];
    let mut buf = vec![0f32; 8 * 512];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        let n = (from_net.readable() as usize).min(512);
        if n > 0 {
            from_net.read(&mut buf[..8 * n]).unwrap();
            if start.elapsed() > Duration::from_millis(500) {
                for f in buf[..8 * n].chunks_exact(8) {
                    for (p, s) in peaks.iter_mut().zip(f) {
                        *p = p.max(s.abs());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        (db(peaks[2]) + 12.0).abs() < 0.2 && (db(peaks[3]) + 12.0).abs() < 0.2,
        "entrées 3-4 : {peaks:?}"
    );
    assert!(
        peaks[0] == 0.0 && peaks[1] == 0.0 && peaks[4..].iter().all(|&p| p == 0.0),
        "autres entrées muettes : {peaks:?}"
    );
    let st = dev.snapshot();
    assert_eq!(st.inputs.len(), 1);
    assert!(st.inputs[0].primed && st.inputs[0].device_channels == vec![3, 4]);

    // Patch : sorties 1-2 → canal 4005 (Standard).
    let r: Value = serde_json::from_str(&shared.handle(
        r#"{"cmd":"patch_output","channel":4005,"name":"MAC 1","format":"standard","device_channels":[1,2]}"#,
        &Caller::trusted("test"),
    ))
    .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    reload(&mut session);

    // Récepteur indépendant du canal 4005.
    let rx_stop = Stop::new();
    let receiver = {
        let (lo, s) = (lo.clone(), rx_stop.clone());
        std::thread::spawn(move || {
            rx::run(
                &lo,
                Channel::new(4005)
                    .unwrap()
                    .group(lw_proto::channel::GroupKind::Stereo),
                5004,
                &s,
                Duration::from_secs(10),
                |_| {},
            )
            .unwrap()
        })
    };
    // Faux plugin : joue 1,5 s de 1 kHz à −6 dBFS sur les sorties 1-2, par blocs de 512 trames.
    let mut to_net = dev.region.producer(Dir::ToNet).unwrap();
    let amp = 10f32.powf(-6.0 / 20.0);
    let t0 = Instant::now();
    let mut n = 0usize;
    while t0.elapsed() < Duration::from_millis(1500) {
        let due = (t0.elapsed().as_secs_f64() * 48_000.0) as usize;
        while n + 512 <= due {
            let block: Vec<f32> = (n..n + 512)
                .flat_map(|i| {
                    let s = amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin();
                    [s, s, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
                })
                .collect();
            to_net.write(&block).unwrap();
            n += 512;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(200));
    rx_stop.request();
    let stats = receiver.join().unwrap();
    assert!(stats.packets > 250, "{} paquets reçus", stats.packets);
    assert_eq!(stats.lost, 0);
    assert_eq!(
        stats.payload_sizes.keys().copied().collect::<Vec<_>>(),
        vec![1440]
    );
    // Crête du dernier intervalle (signal établi) : −6 dBFS.
    assert!(
        (stats.peak_dbfs + 6.0).abs() < 0.2,
        "crête {}",
        stats.peak_dbfs
    );

    omnia_stop.request();
    omnia.join().unwrap();
    session.stop();
    stop.request();
    dev.thread.join().unwrap();
}

/// Patcher et dépatcher des entrées à répétition ne coupe pas un flux émis (patch à chaud).
#[test]
fn input_changes_do_not_interrupt_emission() {
    let lo = iface::list()
        .unwrap()
        .into_iter()
        .find(|i| i.loopback)
        .unwrap();
    let stop = Stop::new();
    let dev = device::start(
        &DeviceConfig {
            channels_to_net: 2,
            channels_from_net: 4,
            ring_frames: 8192,
            loopback: false,
        },
        &stop,
    )
    .unwrap();
    let routes = dev.routes_handle();
    let shared = Shared::new(Some(&lo), 0);
    // Sortie 1-2 émise sur le canal 4011 ; on écoute ce flux pendant toute la séquence.
    let base = r#"{"iface":"lo0","advertise":false,"device":{"channels_to_net":2,"channels_from_net":4},
        "sources":[{"channel":4011,"name":"MAC","format":"standard","device_channels":[1,2]}]"#;
    let cfg = |dest: &str| -> Config {
        serde_json::from_str(&format!("{base},\"destinations\":[{dest}]}}")).unwrap()
    };
    let group = Channel::new(4011)
        .unwrap()
        .group(StreamFormat::Standard.group_kind());
    let listen_stop = Stop::new();
    let ls = listen_stop.clone();
    let lo2 = lo.clone();
    let listener = std::thread::spawn(move || {
        rx::run(&lo2, group, 5004, &ls, Duration::from_secs(10), |_| {}).unwrap()
    });
    std::thread::sleep(Duration::from_millis(200));
    let mut session = Session::start(&cfg(""), lo.clone(), &shared, Some(&routes), &stop).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    for i in 0..10 {
        let dest = match i % 3 {
            0 => r#"{"channel":2,"device_channels":[1,2]}"#,
            1 => r#"{"channel":7,"device_channels":[1,2]},{"channel":2,"device_channels":[3,4]}"#,
            _ => "",
        };
        session.apply(&cfg(dest), &shared, Some(&routes)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    let elapsed = t0.elapsed();
    std::thread::sleep(Duration::from_millis(200));
    listen_stop.request();
    let stats = listener.join().unwrap();
    session.stop();
    stop.request();
    dev.thread.join().unwrap();
    let expected = (elapsed.as_millis() / 5) as u64;
    assert_eq!(stats.resyncs, 0, "flux émis relancé : {stats:?}");
    assert_eq!(stats.lost, 0, "paquets perdus : {stats:?}");
    assert!(
        stats.packets >= expected,
        "{} paquets pour {expected} attendus",
        stats.packets
    );
}
