//! Livewire network detection: listen for advertisements (239.192.255.3:4001) on all
//! candidate Ethernet interfaces for automatic interface selection.
//!
//! - Candidates: IPv4, excluding loopback and virtual interfaces (VPN, containers, virtual
//!   machines, Wi-Fi Direct; per-system lists below). Refresh the list every 5 s
//!   (cable connected, USB adapter added).
//! - An interface is “heard” when it receives a decodable advertisement from another terminal
//!   (advertised address differs from all our addresses).
//! - Selection: keep the active interface while still heard, otherwise use the most recently heard.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lw_proto::adv::Advertisement;
use lw_proto::channel::{ADV_GROUP, ADV_PORT};
use lw_proto::envelope;

use crate::iface::{self, Iface};
use crate::net::rx_socket;
use crate::Stop;

/// An interface unheard for this duration is no longer considered Livewire
/// (three missed keepalives, like the directory).
pub const HEARD_VALIDITY: Duration = Duration::from_secs(75);

/// Virtual interface name prefixes (system name).
#[cfg(target_os = "macos")]
const VIRTUAL_PREFIXES: &[&str] = &["utun", "awdl", "llw", "bridge", "ap", "anpi", "gif", "stf"];
#[cfg(target_os = "linux")]
const VIRTUAL_PREFIXES: &[&str] = &[
    "docker",
    "veth",
    "virbr",
    "br-",
    "tun",
    "tap",
    "wg",
    "vnet",
    "lxc",
    "lxd",
    "cni",
    "flannel",
    "cali",
    "zt",
    "tailscale",
    "podman",
    "vboxnet",
    "vmnet",
];
#[cfg(windows)]
const VIRTUAL_PREFIXES: &[&str] = &[];

/// Lowercase fragments of Windows virtual-interface friendly names: Hyper-V and WSL,
/// hypervisors, VPN, Wi-Fi Direct, Bluetooth. Windows localizes some adapter names: Wi-Fi Direct
/// virtual adapters are “Local Area Connection* N” in English and “Connexion au réseau local* N”
/// in French, so both forms are listed.
#[cfg(windows)]
const VIRTUAL_FRAGMENTS: &[&str] = &[
    "vethernet",
    "hyper-v",
    "virtual",
    "vmware",
    "virtualbox",
    "loopback",
    "tailscale",
    "zerotier",
    "wireguard",
    "openvpn",
    "tap-",
    "wintun",
    "bluetooth",
    "local area connection*",
    "connexion au réseau local*",
];
#[cfg(not(windows))]
const VIRTUAL_FRAGMENTS: &[&str] = &[];

/// Interface potentially carrying Livewire.
pub fn is_candidate(i: &Iface) -> bool {
    let lower = i.friendly.to_lowercase();
    !i.loopback
        && !VIRTUAL_PREFIXES.iter().any(|p| i.name.starts_with(p))
        && !VIRTUAL_FRAGMENTS.iter().any(|f| lower.contains(f))
}

/// Last advertisement heard per interface (system name).
#[derive(Clone, Default)]
pub struct Heard {
    inner: Arc<Mutex<BTreeMap<String, Instant>>>,
}

impl Heard {
    pub fn note(&self, iface: &str, at: Instant) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(iface.to_string(), at);
    }

    /// Has the interface heard Livewire recently?
    pub fn recent(&self, iface: &str, now: Instant) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(iface)
            .is_some_and(|t| now.duration_since(*t) < HEARD_VALIDITY)
    }

    /// Automatic selection: `current` if still heard, otherwise the most recently heard.
    pub fn choose(&self, current: Option<&str>, now: Instant) -> Option<String> {
        if let Some(c) = current {
            if self.recent(c, now) {
                return Some(c.to_string());
            }
        }
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.iter()
            .filter(|(_, t)| now.duration_since(**t) < HEARD_VALIDITY)
            .max_by_key(|(_, t)| **t)
            .map(|(n, _)| n.clone())
    }
}

/// Listen until stopped and update `heard`.
pub fn run(heard: &Heard, stop: &Stop) {
    let mut sockets: BTreeMap<String, (Ipv4Addr, UdpSocket)> = BTreeMap::new();
    let mut local: Vec<Ipv4Addr> = Vec::new();
    let mut last_scan: Option<Instant> = None;
    let mut buf = [0u8; 2048];
    while !stop.requested() {
        if last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
            last_scan = Some(Instant::now());
            let list = iface::list().unwrap_or_default();
            local = list.iter().map(|i| i.ipv4).collect();
            // Disappeared interfaces or changed addresses: close socket, reopen as needed.
            sockets.retain(|name, (ip, _)| list.iter().any(|i| &i.name == name && i.ipv4 == *ip));
            for i in list.iter().filter(|i| is_candidate(i)) {
                if sockets.contains_key(&i.name) {
                    continue;
                }
                match rx_socket(i, ADV_GROUP, ADV_PORT, Duration::from_millis(1)) {
                    Ok(s) if s.set_nonblocking(true).is_ok() => {
                        sockets.insert(i.name.clone(), (i.ipv4, s));
                    }
                    // Interface without multicast (e.g. point-to-point): ignore until the next scan.
                    _ => {}
                }
            }
        }
        for (name, (_, s)) in &sockets {
            while let Ok((n, _)) = s.recv_from(&mut buf) {
                let Some(data) = buf.get(..n) else { break };
                let Ok((_, msg)) = envelope::decode(data) else {
                    continue;
                };
                let Ok(adv) = Advertisement::from_msg(&msg) else {
                    continue;
                };
                if !local.contains(&adv.terminal.ip) {
                    heard.note(name, Instant::now());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(name: &str, loopback: bool) -> Iface {
        Iface {
            name: name.into(),
            friendly: name.into(),
            index: 1,
            ipv4: Ipv4Addr::new(192, 168, 2, 10),
            loopback,
        }
    }

    #[cfg(target_os = "macos")]
    const NAMES: (&[&str], &[&str]) = (&["en7", "en0"], &["utun3", "bridge100", "awdl0"]);
    #[cfg(target_os = "linux")]
    const NAMES: (&[&str], &[&str]) = (
        &["enp3s0", "eth0", "eno1"],
        &[
            "docker0",
            "veth12ab",
            "virbr0",
            "br-3f2a",
            "wg0",
            "tailscale0",
        ],
    );
    #[cfg(windows)]
    const NAMES: (&[&str], &[&str]) = (
        &["Ethernet", "Ethernet 2", "Studio LAN"],
        &[
            "vEthernet (WSL)",
            "VirtualBox Host-Only Network",
            "Local Area Connection* 1",
            "Tailscale",
            "Bluetooth Network Connection",
        ],
    );

    #[test]
    fn candidates_exclude_virtual_and_loopback() {
        let (real, virtual_) = NAMES;
        for n in real {
            assert!(is_candidate(&nic(n, false)), "{n}");
        }
        for n in virtual_ {
            assert!(!is_candidate(&nic(n, false)), "{n}");
        }
        assert!(!is_candidate(&nic("lo", true)));
    }

    #[test]
    fn choice_is_sticky_then_most_recent() {
        let h = Heard::default();
        let t0 = Instant::now();
        assert_eq!(h.choose(None, t0), None);
        h.note("en7", t0);
        h.note("en8", t0 + Duration::from_secs(10));
        let t = t0 + Duration::from_secs(20);
        assert_eq!(h.choose(None, t).as_deref(), Some("en8"));
        assert_eq!(
            h.choose(Some("en7"), t).as_deref(),
            Some("en7"),
            "current interface kept"
        );
        let late = t0 + Duration::from_secs(80);
        assert_eq!(
            h.choose(Some("en7"), late).as_deref(),
            Some("en8"),
            "en7 silent: switch"
        );
        assert_eq!(h.choose(None, t0 + Duration::from_secs(200)), None);
    }
}
