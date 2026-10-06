//! End-to-end discovery on lo0: advertiser acts as a remote terminal (mock IP),
//! daemon discovery must reconstruct its ten sources (two pages).

#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use lw_daemon::advertise::Advertiser;
use lw_daemon::discovery::{self, Directory};
use lw_daemon::{iface, Stop};
use lw_proto::adv::{AdvStreamType, Source};
use lw_proto::channel::Channel;

#[test]
fn discovery_rebuilds_remote_terminal_sources() {
    let lo = iface::list()
        .unwrap()
        .into_iter()
        .find(|i| i.loopback)
        .unwrap();
    let dir = Directory::new();
    let stop = Stop::new();
    let listener = {
        let (lo, dir, stop) = (lo.clone(), dir.clone(), stop.clone());
        std::thread::spawn(move || discovery::run(&lo, &dir, &stop))
    };
    std::thread::sleep(Duration::from_millis(200));
    let sources: Vec<Source> = (1..=10)
        .map(|i| {
            Source::new(
                i,
                Channel::new(20 + i).unwrap(),
                &format!("STUDIO-A {i}"),
                AdvStreamType::StereoL24,
            )
        })
        .collect();
    let mut adv = Advertiser::new(&lo, "STUDIO-A", sources).unwrap();
    adv.terminal.ip = Ipv4Addr::new(10, 9, 9, 9); // Otherwise ignored as our own advertisements
    adv.send_full(|| Duration::from_millis(5), &Stop::new())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while dir.sources(Instant::now()).len() < 10 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let found = dir.sources(Instant::now());
    stop.request();
    listener.join().unwrap().unwrap();
    assert_eq!(found.len(), 10);
    assert_eq!(
        (
            found[0].channel,
            found[0].name.as_str(),
            found[0].terminal.as_str()
        ),
        (21, "STUDIO-A 1", "STUDIO-A")
    );
    assert_eq!(found[9].stream, "239.192.0.30");
    assert_eq!(found[9].terminal_ip, "10.9.9.9");
}
