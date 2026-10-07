//! Local loopback: transmission → reception, advertisement → decoding.
//! Validate interface-bound sockets, pacing, packetization, and advertisements without external equipment.

#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;
use std::time::Duration;

use lw_daemon::advertise::Advertiser;
use lw_daemon::iface::{self, Iface};
use lw_daemon::net::{rx_socket, TxOptions};
use lw_daemon::rx::RxStats;
use lw_daemon::{rx, tx, Stop};
use lw_proto::adv::{AdvStreamType, Advertisement, Source};
use lw_proto::channel::{Channel, ADV_GROUP, ADV_PORT};
use lw_proto::envelope;
use lw_proto::format::StreamFormat;

fn loopback() -> Iface {
    iface::list()
        .unwrap()
        .into_iter()
        .find(|i| i.loopback)
        .expect("loopback interface")
}

/// Transmit `seconds` seconds in the given format and return receive statistics.
fn roundtrip(channel: u16, format: StreamFormat, seconds: f64) -> (tx::TxReport, RxStats) {
    let lo = loopback();
    let stream = tx::TxStream::new(Channel::new(channel).unwrap(), format);
    let group = stream.group();
    let rx_stop = Stop::new();
    let rx_handle = {
        let (lo, stop) = (lo.clone(), rx_stop.clone());
        std::thread::spawn(move || {
            rx::run(&lo, group, 5004, &stop, Duration::from_secs(10), |_| {}).unwrap()
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    let tx_stop = Stop::new();
    tx_stop.after(Duration::from_secs_f64(seconds));
    let report = tx::run(&lo, &stream, TxOptions::default(), &tx_stop).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    rx_stop.request();
    (report, rx_handle.join().unwrap())
}

#[test]
fn standard_stream_roundtrip() {
    let (tx, rx) = roundtrip(4001, StreamFormat::Standard, 1.0);
    assert!(
        (195..=207).contains(&tx.packets),
        "200 packets/s expected, {} transmitted",
        tx.packets
    );
    assert_eq!(
        rx.packets, tx.packets,
        "everything transmitted must be received on lo0"
    );
    assert_eq!(
        (rx.lost, rx.late_or_dup, rx.resyncs, rx.invalid),
        (0, 0, 0, 0)
    );
    assert_eq!(
        rx.payload_sizes.keys().copied().collect::<Vec<_>>(),
        vec![1440]
    );
    assert_eq!(rx.ts_steps.keys().copied().collect::<Vec<_>>(), vec![240]);
    assert_eq!(
        rx.payload_types.keys().copied().collect::<Vec<_>>(),
        vec![96]
    );
    assert!(rx.ssrc_is_group, "SSRC = group");
    assert!(
        (rx.peak_dbfs + 20.0).abs() < 0.2,
        "peak {} dBFS",
        rx.peak_dbfs
    );
}

#[test]
fn aes67_stream_roundtrip() {
    let (tx, rx) = roundtrip(4002, StreamFormat::Aes67, 1.0);
    assert!(
        (980..=1030).contains(&tx.packets),
        "1000 packets/s expected, {} transmitted",
        tx.packets
    );
    assert_eq!(rx.packets, tx.packets);
    assert_eq!((rx.lost, rx.resyncs), (0, 0));
    assert_eq!(
        rx.payload_sizes.keys().copied().collect::<Vec<_>>(),
        vec![288]
    );
    assert_eq!(rx.ts_steps.keys().copied().collect::<Vec<_>>(), vec![48]);
    eprintln!(
        "AES67: {} packets, {} late > 1 ms, max lateness {} µs",
        tx.packets, tx.late_packets, tx.max_late_us
    );
    // Scheduling delay depends on the OS (normal-priority thread): measured, not required.
    // Daemon audio threads will use real-time scheduling (THREAD_TIME_CONSTRAINT_POLICY, C layer).
}

#[test]
fn surround_goes_to_239_196() {
    let (tx, rx) = roundtrip(5, StreamFormat::Surround, 0.3);
    assert_eq!(tx.group, "239.196.0.5");
    assert_eq!(
        rx.payload_sizes.keys().copied().collect::<Vec<_>>(),
        vec![1440]
    );
    assert_eq!(rx.ts_steps.keys().copied().collect::<Vec<_>>(), vec![60]);
}

#[test]
fn advertisement_is_decodable() {
    let lo = loopback();
    let sock = rx_socket(&lo, ADV_GROUP, ADV_PORT, Duration::from_millis(500)).unwrap();
    let sources: Vec<Source> = (1..=10)
        .map(|i| {
            Source::new(
                i,
                Channel::new(4000 + i).unwrap(),
                &format!("MAC {i}"),
                AdvStreamType::StereoL24,
            )
        })
        .collect();
    let mut adv = Advertiser::new(&lo, "openlw-test", sources).unwrap();
    adv.send_full(|| Duration::from_millis(5), &Stop::new())
        .unwrap();
    adv.send_short().unwrap();
    let mut buf = [0u8; 2048];
    let mut pages = Vec::new();
    while let Ok((n, _)) = sock.recv_from(&mut buf) {
        let (h, msg) = envelope::decode(&buf[..n]).unwrap();
        assert_eq!((h.layer, h.tlv_version), (3, 2));
        pages.push(Advertisement::from_msg(&msg).unwrap());
    }
    assert_eq!(pages.len(), 3, "2 full pages (8 + 2 sources) + 1 short");
    assert!(pages[0].full && pages[1].full && !pages[2].full);
    assert_eq!(pages[0].sources.len() + pages[1].sources.len(), 10);
    assert!(pages
        .iter()
        .all(|p| p.terminal.nums == 10 && p.terminal.ip == Ipv4Addr::LOCALHOST));
    assert_eq!(pages[1].sources[1].name, "MAC 10");
    assert_eq!(pages[1].sources[1].stream, Ipv4Addr::new(239, 192, 15, 170));
}
