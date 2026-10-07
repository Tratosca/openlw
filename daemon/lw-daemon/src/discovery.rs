//! Livewire source discovery: advertisement listener (ADV) and directory (`docs/protocol/03-advertisement.md`).
//!
//! - Listen on 239.192.255.3:4001 through the selected interface; decode full and short advertisements.
//! - Full advertisements arrive as eight-source pages: the directory accumulates them for a shared
//!   advertisement version (`ADVV`) and starts afresh when the version changes.
//! - Keepalive from a terminal without a known full advertisement: `READ` to `INIP:UDPC`, at most once per 5 s per terminal.
//! - Terminal silent for 75 s (three missed keepalives): remove from directory.

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

/// Silent-terminal expiry delay.
pub const EXPIRY: Duration = Duration::from_secs(75);
/// Minimum interval between `READ` requests to the same terminal.
pub const REQUEST_INTERVAL: Duration = Duration::from_secs(5);

/// Source observed on the network.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiscoveredSource {
    /// Livewire channel (`PSID`).
    pub channel: u32,
    pub name: String,
    /// Advertised stream group (`FSID`).
    pub stream: String,
    /// stereo, stereo-l16, surround, or numeric code.
    pub kind: String,
    pub shareable: bool,
    pub terminal: String,
    pub terminal_ip: String,
    /// Seconds since the terminal was last heard.
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
    /// Current advertisement-version sources, by slot.
    sources: BTreeMap<u16, lw_proto::adv::Source>,
}

impl TerminalEntry {
    fn complete(&self) -> bool {
        self.sources.len() >= usize::from(self.nums) && self.name.is_some()
    }
}

/// Shared directory.
#[derive(Clone, Default)]
pub struct Directory {
    inner: Arc<Mutex<BTreeMap<Ipv4Addr, TerminalEntry>>>,
}

/// Action requested by the directory after an advertisement.
#[derive(Debug, PartialEq, Eq)]
pub enum Followup {
    None,
    /// Send a `READ` request to this terminal.
    RequestFull(SocketAddrV4),
}

impl Directory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Incorporate an advertisement received at `now`; indicate whether to request a full advertisement.
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
            // New advertisement version: previous list is no longer valid.
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

    /// Remove terminals silent longer than [`EXPIRY`].
    pub fn expire(&self, now: Instant) {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, e| now.duration_since(e.last_heard) < EXPIRY);
    }

    /// Known sources, sorted by channel.
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

    /// Heard terminals, including those whose source lists are not yet known.
    pub fn terminals(&self) -> Vec<(String, Option<String>, usize, u16)> {
        let map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        map.iter()
            .map(|(ip, e)| (ip.to_string(), e.name.clone(), e.sources.len(), e.nums))
            .collect()
    }
}

/// Valid Livewire channel of a discovered source (for patching).
pub fn channel_of(s: &DiscoveredSource) -> Option<Channel> {
    u16::try_from(s.channel).ok().and_then(Channel::new)
}

/// Listen for advertisements until stopped.
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
                    continue; // Our own advertisements
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
        // Unknown terminal keepalive: request full advertisement from INIP:UDPC.
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
            "at most one request per 5 s"
        );
        // Two pages (8 + 2 sources).
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
        // Complete list: keepalives no longer request anything.
        assert_eq!(
            d.ingest(&short, t0 + Duration::from_secs(30)),
            Followup::None
        );
        // New advertisement version: reset list, request again.
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
