//! Découverte des sources Livewire : écoute des annonces (ADV) et annuaire (`docs/protocol/03-advertisement.md`).
//!
//! - Écoute 239.192.255.3:4001 sur l'interface choisie ; décode annonces complètes et courtes.
//! - Une annonce complète arrive en pages de 8 sources : l'annuaire les cumule pour une même
//!   version d'annonce (`ADVV`) et repart de zéro quand la version change.
//! - Terminal entendu en keepalive sans annonce complète connue : requête `READ` vers `INIP:UDPC`, au plus une toutes les 5 s par terminal.
//! - Terminal muet depuis 75 s (3 keepalives manqués) : retiré de l'annuaire.

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lw_proto::adv::{full_info_request, AdvStreamType, Advertisement};
use lw_proto::channel::{Channel, ADV_GROUP, ADV_PORT};
use lw_proto::envelope::{self, Header};
use serde::Serialize;

use crate::iface::Iface;
use crate::net::{rx_socket, tx_socket, TxOptions};
use crate::Stop;

/// Délai d'expiration d'un terminal silencieux.
pub const EXPIRY: Duration = Duration::from_secs(75);
/// Intervalle minimal entre deux requêtes `READ` vers un même terminal.
pub const REQUEST_INTERVAL: Duration = Duration::from_secs(5);

/// Source vue sur le réseau.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiscoveredSource {
    /// Canal Livewire (`PSID`).
    pub channel: u32,
    pub name: String,
    /// Groupe du flux annoncé (`FSID`).
    pub stream: String,
    /// stereo, stereo-l16, surround ou code numérique.
    pub kind: String,
    pub shareable: bool,
    pub terminal: String,
    pub terminal_ip: String,
    /// Secondes depuis le dernier signe de vie du terminal.
    pub age_s: u64,
}

#[derive(Debug, Clone)]
struct TerminalEntry {
    name: Option<String>,
    advv: u32,
    nums: u16,
    control_port: u16,
    last_heard: Instant,
    last_request: Option<Instant>,
    /// Sources de la version d'annonce courante, par emplacement.
    sources: BTreeMap<u16, lw_proto::adv::Source>,
}

impl TerminalEntry {
    fn complete(&self) -> bool {
        self.sources.len() >= usize::from(self.nums) && self.name.is_some()
    }
}

/// Annuaire partagé.
#[derive(Clone, Default)]
pub struct Directory {
    inner: Arc<Mutex<BTreeMap<Ipv4Addr, TerminalEntry>>>,
}

/// Action demandée par l'annuaire après une annonce.
#[derive(Debug, PartialEq, Eq)]
pub enum Followup {
    None,
    /// Envoyer une requête `READ` à ce terminal.
    RequestFull(SocketAddrV4),
}

impl Directory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intègre une annonce reçue à `now` ; indique s'il faut demander l'annonce complète.
    pub fn ingest(&self, adv: &Advertisement, now: Instant) -> Followup {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let t = &adv.terminal;
        let entry = map.entry(t.ip).or_insert_with(|| TerminalEntry {
            name: None,
            advv: t.advv,
            nums: t.nums,
            control_port: t.control_port,
            last_heard: now,
            last_request: None,
            sources: BTreeMap::new(),
        });
        entry.last_heard = now;
        entry.control_port = t.control_port;
        if entry.advv != t.advv {
            // Nouvelle version d'annonce : la liste précédente n'est plus valable.
            entry.advv = t.advv;
            entry.sources.clear();
            entry.name = None;
        }
        entry.nums = t.nums;
        if adv.full {
            if t.name.is_some() {
                entry.name = t.name.clone();
            }
            for s in &adv.sources {
                entry.sources.insert(s.slot, s.clone());
            }
            return Followup::None;
        }
        let due = entry
            .last_request
            .is_none_or(|at| now.duration_since(at) >= REQUEST_INTERVAL);
        if !entry.complete() && due {
            entry.last_request = Some(now);
            return Followup::RequestFull(SocketAddrV4::new(t.ip, t.control_port));
        }
        Followup::None
    }

    /// Retire les terminaux muets depuis plus de [`EXPIRY`].
    pub fn expire(&self, now: Instant) {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, e| now.duration_since(e.last_heard) < EXPIRY);
    }

    /// Sources connues, triées par canal.
    pub fn sources(&self, now: Instant) -> Vec<DiscoveredSource> {
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<DiscoveredSource> = map
            .iter()
            .flat_map(|(ip, e)| {
                e.sources.values().map(move |s| DiscoveredSource {
                    channel: s.channel,
                    name: s.name.clone(),
                    stream: s.stream.to_string(),
                    kind: match s.stream_type {
                        AdvStreamType::StereoL24 => "stereo".into(),
                        AdvStreamType::StereoL16 => "stereo-l16".into(),
                        AdvStreamType::Surround => "surround".into(),
                        AdvStreamType::Other(c) => format!("type {c}"),
                    },
                    shareable: s.shareable != 0,
                    terminal: e.name.clone().unwrap_or_else(|| ip.to_string()),
                    terminal_ip: ip.to_string(),
                    age_s: now.duration_since(e.last_heard).as_secs(),
                })
            })
            .collect();
        out.sort_by_key(|s| s.channel);
        out
    }

    /// Terminaux entendus, y compris ceux dont la liste de sources n'est pas encore connue.
    pub fn terminals(&self) -> Vec<(String, Option<String>, usize, u16)> {
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.iter()
            .map(|(ip, e)| (ip.to_string(), e.name.clone(), e.sources.len(), e.nums))
            .collect()
    }
}

/// Canal Livewire valide d'une source découverte (pour patcher).
pub fn channel_of(s: &DiscoveredSource) -> Option<Channel> {
    u16::try_from(s.channel).ok().and_then(Channel::new)
}

/// Écoute les annonces jusqu'à l'arrêt.
pub fn run(iface: &Iface, directory: &Directory, stop: &Stop) -> io::Result<()> {
    let sock = rx_socket(iface, ADV_GROUP, ADV_PORT, Duration::from_millis(250))?;
    let req_sock: UdpSocket = tx_socket(
        iface,
        0,
        TxOptions {
            ttl: 64,
            tos: 0,
            realtime: false,
        },
    )?;
    let mut seq = 0u32;
    let mut buf = [0u8; 2048];
    let mut last_expire = Instant::now();
    while !stop.requested() {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                let Some(data) = buf.get(..n) else { continue };
                let Ok((_, msg)) = envelope::decode(data) else {
                    continue;
                };
                let Ok(adv) = Advertisement::from_msg(&msg) else {
                    continue;
                };
                if adv.terminal.ip == iface.ipv4 {
                    continue; // nos propres annonces
                }
                if let Followup::RequestFull(dest) = directory.ingest(&adv, Instant::now()) {
                    seq = seq.wrapping_add(1).max(1);
                    if let Ok(raw) = envelope::encode(&Header::datagram(seq), &full_info_request())
                    {
                        let _ = req_sock.send_to(&raw, dest);
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        if last_expire.elapsed() >= Duration::from_secs(1) {
            directory.expire(Instant::now());
            last_expire = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use lw_proto::adv::{Source, Terminal};

    fn terminal(advv: u32, nums: u16) -> Terminal {
        let mut t = Terminal::new(advv, Ipv4Addr::new(192, 168, 2, 50), "STUDIO-A");
        t.nums = nums;
        t
    }

    fn src(slot: u16, ch: u16, name: &str) -> Source {
        Source::new(
            slot,
            Channel::new(ch).unwrap(),
            name,
            AdvStreamType::StereoL24,
        )
    }

    #[test]
    fn pages_accumulate_and_short_triggers_request_once() {
        let d = Directory::new();
        let t0 = Instant::now();
        // Keepalive d'un inconnu : demande d'annonce complète vers INIP:UDPC.
        let short = Advertisement {
            full: false,
            terminal: terminal(3, 10),
            sources: vec![],
        };
        assert_eq!(
            d.ingest(&short, t0),
            Followup::RequestFull(SocketAddrV4::new(Ipv4Addr::new(192, 168, 2, 50), 4000))
        );
        assert_eq!(
            d.ingest(&short, t0 + Duration::from_secs(1)),
            Followup::None,
            "pas plus d'une requête par 5 s"
        );
        // Deux pages (8 + 2 sources).
        let p1 = Advertisement {
            full: true,
            terminal: terminal(3, 10),
            sources: (1..=8).map(|i| src(i, 100 + i, "A")).collect(),
        };
        let p2 = Advertisement {
            full: true,
            terminal: terminal(3, 10),
            sources: vec![src(9, 109, "B"), src(10, 110, "C")],
        };
        d.ingest(&p1, t0);
        d.ingest(&p2, t0);
        let s = d.sources(t0);
        assert_eq!(s.len(), 10);
        assert_eq!(
            (s[0].channel, s[0].terminal.as_str(), s[9].name.as_str()),
            (101, "STUDIO-A", "C")
        );
        assert_eq!(s[0].stream, "239.192.0.101");
        // Liste complète : un keepalive ne redemande plus rien.
        assert_eq!(
            d.ingest(&short, t0 + Duration::from_secs(30)),
            Followup::None
        );
        // Nouvelle version d'annonce : liste remise à zéro, nouvelle demande.
        let short4 = Advertisement {
            full: false,
            terminal: terminal(4, 10),
            sources: vec![],
        };
        assert!(matches!(
            d.ingest(&short4, t0 + Duration::from_secs(40)),
            Followup::RequestFull(_)
        ));
        assert!(d.sources(t0).is_empty());
    }

    #[test]
    fn silent_terminals_expire() {
        let d = Directory::new();
        let t0 = Instant::now();
        d.ingest(
            &Advertisement {
                full: true,
                terminal: terminal(1, 1),
                sources: vec![src(1, 5, "X")],
            },
            t0,
        );
        d.expire(t0 + Duration::from_secs(60));
        assert_eq!(d.sources(t0).len(), 1);
        d.expire(t0 + Duration::from_secs(80));
        assert!(d.sources(t0).is_empty());
    }
}
