//! Annonce des sources (ADV) sur 239.192.255.3:4001, cadence des appareils Livewire (`docs/protocol/03-advertisement.md`).
//!
//! Annonce complète au démarrage puis à 1 s ± 0,5 s ; ensuite une courte toutes les 20 s ± 5 s ;
//! complète après 8 courtes ou à chaque changement. Pages de 8 sources espacées de 100 ms ± 50 ms.

use std::io;
use std::net::{SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use lw_proto::adv::{Advertisement, Source, Terminal};
use lw_proto::channel::{ADV_GROUP, ADV_PORT};
use lw_proto::envelope::{self, Header};

use crate::iface::Iface;
use crate::net::{tx_socket, TxOptions};
use crate::{Jitter, Stop};

/// Version d'annonce (`ADVV`) d'une nouvelle session : secondes Unix, donc différente et croissante
/// à chaque relance. Un appareil qui connaît déjà la version 1 ignorerait sinon la nouvelle liste.
fn session_advv() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(1)
        .max(1)
}

pub struct Advertiser {
    sock: UdpSocket,
    dest: SocketAddrV4,
    pub terminal: Terminal,
    pub sources: Vec<Source>,
    seq: u32,
    pub sent_full: u64,
    pub sent_short: u64,
}

impl Advertiser {
    pub fn new(iface: &Iface, terminal_name: &str, sources: Vec<Source>) -> io::Result<Self> {
        // TTL 128 et TOS 0 ; port source éphémère.
        let sock = tx_socket(
            iface,
            0,
            TxOptions {
                ttl: 128,
                tos: 0,
                realtime: false,
            },
        )?;
        Ok(Self {
            sock,
            dest: SocketAddrV4::new(ADV_GROUP, ADV_PORT),
            terminal: Terminal::new(session_advv(), iface.ipv4, terminal_name),
            sources,
            seq: 0,
            sent_full: 0,
            sent_short: 0,
        })
    }

    fn send(&mut self, adv: &Advertisement) -> io::Result<()> {
        self.seq = self.seq.wrapping_add(1).max(1);
        let msg = adv
            .to_msg()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let raw = envelope::encode(&Header::datagram(self.seq), &msg)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.sock.send_to(&raw, self.dest)?;
        Ok(())
    }

    /// Envoie toutes les pages d'une annonce complète (`page_gap` entre pages).
    pub fn send_full(
        &mut self,
        mut page_gap: impl FnMut() -> Duration,
        stop: &Stop,
    ) -> io::Result<()> {
        let pages = Advertisement::full_pages(&self.terminal, &self.sources);
        let n = pages.len();
        for (i, page) in pages.iter().enumerate() {
            if stop.requested() {
                break;
            }
            self.send(page)?;
            if i + 1 < n {
                std::thread::sleep(page_gap());
            }
        }
        self.sent_full += 1;
        Ok(())
    }

    pub fn send_short(&mut self) -> io::Result<()> {
        let mut t = self.terminal.clone();
        t.nums = self.sources.len() as u16;
        self.send(&Advertisement {
            full: false,
            terminal: t,
            sources: Vec::new(),
        })?;
        self.sent_short += 1;
        Ok(())
    }

    /// Change la liste des sources : nouvelle version d'annonce, complète immédiate au prochain tour.
    pub fn set_sources(&mut self, sources: Vec<Source>) {
        self.sources = sources;
        self.terminal.advv = self.terminal.advv.wrapping_add(1);
    }

    /// Boucle d'annonce jusqu'à l'arrêt.
    pub fn run(&mut self, stop: &Stop) -> io::Result<()> {
        let mut jitter = Jitter::seeded();
        let mut shorts = 0u32;
        let mut first = true;
        let mut full_due = true;
        let mut next = Instant::now();
        while !stop.requested() {
            if Instant::now() < next {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            if full_due {
                let mut j = Jitter::seeded();
                self.send_full(
                    || j.around(Duration::from_millis(100), Duration::from_millis(50)),
                    stop,
                )?;
                shorts = 0;
                full_due = first;
                next = Instant::now()
                    + if first {
                        jitter.around(Duration::from_secs(1), Duration::from_millis(500))
                    } else {
                        jitter.around(Duration::from_secs(20), Duration::from_secs(5))
                    };
                first = false;
            } else {
                self.send_short()?;
                shorts += 1;
                full_due = shorts >= 8;
                next =
                    Instant::now() + jitter.around(Duration::from_secs(20), Duration::from_secs(5));
            }
        }
        Ok(())
    }
}
