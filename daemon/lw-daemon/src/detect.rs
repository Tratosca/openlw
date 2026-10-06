//! Détection du réseau Livewire : écoute des annonces (239.192.255.3:4001) sur toutes les
//! interfaces Ethernet candidates, pour le choix automatique de l'interface.
//!
//! - Candidates : IPv4, hors bouclage et hors interfaces virtuelles (utun, awdl, llw, bridge, ap,
//!   anpi, gif, stf). La liste est relue toutes les 5 s (câble branché, adaptateur USB ajouté).
//! - Une interface est « entendue » quand elle reçoit une annonce décodable d'un autre terminal
//!   (adresse annoncée différente de toutes nos adresses).
//! - Choix : l'interface déjà utilisée tant qu'elle reste entendue, sinon la plus récemment entendue.

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

/// Une interface non entendue depuis cette durée n'est plus considérée comme Livewire
/// (3 keepalives manqués, comme l'annuaire).
pub const HEARD_VALIDITY: Duration = Duration::from_secs(75);

const VIRTUAL_PREFIXES: [&str; 8] = ["utun", "awdl", "llw", "bridge", "ap", "anpi", "gif", "stf"];

/// Interface susceptible de porter Livewire.
pub fn is_candidate(i: &Iface) -> bool {
    !i.loopback && !VIRTUAL_PREFIXES.iter().any(|p| i.name.starts_with(p))
}

/// Dernière annonce entendue par interface (nom BSD).
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

    /// L'interface a-t-elle entendu Livewire récemment ?
    pub fn recent(&self, iface: &str, now: Instant) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(iface)
            .is_some_and(|t| now.duration_since(*t) < HEARD_VALIDITY)
    }

    /// Choix automatique : `current` s'il est encore entendu, sinon la plus récemment entendue.
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

/// Écoute jusqu'à l'arrêt et met `heard` à jour.
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
            // Interfaces disparues ou changées d'adresse : socket fermée, rouverte au besoin.
            sockets.retain(|name, (ip, _)| list.iter().any(|i| &i.name == name && i.ipv4 == *ip));
            for i in list.iter().filter(|i| is_candidate(i)) {
                if sockets.contains_key(&i.name) {
                    continue;
                }
                match rx_socket(i, ADV_GROUP, ADV_PORT, Duration::from_millis(1)) {
                    Ok(s) if s.set_nonblocking(true).is_ok() => {
                        sockets.insert(i.name.clone(), (i.ipv4, s));
                    }
                    // Interface sans multicast (ex. point à point) : ignorée jusqu'au prochain examen.
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

    #[test]
    fn candidates_exclude_virtual_and_loopback() {
        assert!(is_candidate(&nic("en7", false)));
        assert!(!is_candidate(&nic("lo0", true)));
        assert!(!is_candidate(&nic("utun3", false)));
        assert!(!is_candidate(&nic("bridge100", false)));
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
            "interface en cours conservée"
        );
        let late = t0 + Duration::from_secs(80);
        assert_eq!(
            h.choose(Some("en7"), late).as_deref(),
            Some("en8"),
            "en7 muette : bascule"
        );
        assert_eq!(h.choose(None, t0 + Duration::from_secs(200)), None);
    }
}
