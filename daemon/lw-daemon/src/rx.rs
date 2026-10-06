//! Réception d'un flux RTP Livewire / AES67 et statistiques : paquets, pertes, erreurs de séquence,
//! gigue (RFC 3550).

use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use lw_proto::rtp::{decode_l24, Packet};
use serde::Serialize;

use crate::iface::Iface;
use crate::net::rx_socket;
use crate::Stop;

/// Statistiques cumulées d'un flux reçu.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RxStats {
    pub group: String,
    pub packets: u64,
    pub bytes: u64,
    pub invalid: u64,
    /// Paquets manquants (sauts de séquence vers l'avant).
    pub lost: u64,
    /// Doublons ou paquets en retard (écart dans [-399, 0]).
    pub late_or_dup: u64,
    /// Ruptures de séquence hors fenêtre (nouveau flux, redémarrage de l'émetteur).
    pub resyncs: u64,
    pub payload_types: BTreeMap<u8, u64>,
    pub payload_sizes: BTreeMap<usize, u64>,
    pub ts_steps: BTreeMap<u32, u64>,
    pub ssrc: Option<u32>,
    pub ssrc_is_group: bool,
    /// Gigue RFC 3550 en échantillons à 48 kHz.
    pub jitter_samples: f64,
    /// Crête depuis le dernier relevé, dBFS (L24 supposé).
    pub peak_dbfs: f64,
    pub senders: BTreeMap<String, u64>,
    #[serde(skip)]
    prev: Option<(Instant, u16, u32)>,
    #[serde(skip)]
    peak: u32,
}

impl RxStats {
    fn new(group: Ipv4Addr) -> Self {
        Self {
            group: group.to_string(),
            peak_dbfs: f64::NEG_INFINITY,
            ..Self::default()
        }
    }

    /// Prend en compte un paquet reçu à `now`. Renvoie `true` si le paquet est retenu (ni invalide,
    /// ni doublon, ni en retard) ; ses échantillons L24 sont alors laissés dans `scratch`.
    pub fn record(
        &mut self,
        now: Instant,
        sender: Ipv4Addr,
        data: &[u8],
        scratch: &mut Vec<i32>,
    ) -> bool {
        scratch.clear();
        let Ok(pkt) = Packet::parse(data) else {
            self.invalid += 1;
            return false;
        };
        self.packets += 1;
        self.bytes += data.len() as u64;
        *self.payload_types.entry(pkt.payload_type).or_default() += 1;
        *self.payload_sizes.entry(pkt.payload.len()).or_default() += 1;
        *self.senders.entry(sender.to_string()).or_default() += 1;
        self.ssrc = Some(pkt.ssrc);
        self.ssrc_is_group = self.group.parse::<Ipv4Addr>().map(u32::from).ok() == Some(pkt.ssrc);
        if let Some((t0, seq0, ts0)) = self.prev {
            let dseq = pkt.sequence.wrapping_sub(seq0) as i16;
            match dseq {
                1 => {}
                d if (-399..=0).contains(&d) => {
                    self.late_or_dup += 1;
                    return false;
                }
                d if d > 1 && d < 400 => self.lost += (d - 1) as u64,
                _ => self.resyncs += 1,
            }
            let dts = pkt.timestamp.wrapping_sub(ts0);
            *self.ts_steps.entry(dts).or_default() += 1;
            let arrival = now.duration_since(t0).as_secs_f64() * 48_000.0;
            let d = (arrival - f64::from(dts)).abs();
            self.jitter_samples += (d - self.jitter_samples) / 16.0;
        }
        self.prev = Some((now, pkt.sequence, pkt.timestamp));
        decode_l24(pkt.payload, scratch);
        let p = scratch.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        self.peak = self.peak.max(p);
        true
    }

    /// Fige la crête courante en dBFS et la remet à zéro (appel périodique).
    pub fn take_peak(&mut self) {
        self.peak_dbfs = if self.peak == 0 {
            f64::NEG_INFINITY
        } else {
            20.0 * (f64::from(self.peak) / 8_388_607.0).log10()
        };
        self.peak = 0;
    }
}

/// Reçoit `group:port` jusqu'à l'arrêt ; `on_report` est appelé environ chaque `every`.
pub fn run(
    iface: &Iface,
    group: Ipv4Addr,
    port: u16,
    stop: &Stop,
    every: Duration,
    on_report: impl FnMut(&RxStats),
) -> io::Result<RxStats> {
    run_into(iface, group, port, stop, every, None, on_report)
}

/// Comme [`run`], et pousse l'audio décodé (L24 → float) dans `sink` (entrées du périphérique).
/// Les paquets dont la taille ne correspond pas au nombre de canaux du bus sont ignorés.
pub fn run_into(
    iface: &Iface,
    group: Ipv4Addr,
    port: u16,
    stop: &Stop,
    every: Duration,
    mut sink: Option<crate::bus::BusWriter>,
    mut on_report: impl FnMut(&RxStats),
) -> io::Result<RxStats> {
    let mut floats: Vec<f32> = Vec::with_capacity(2048);
    let sock = rx_socket(iface, group, port, Duration::from_millis(50))?;
    // Flux patché vers le périphérique : une réception retardée de quelques dizaines de ms (thread à
    // priorité normale) vide le tampon de gigue puis le fait déborder (manque, puis glissement).
    // Le thread bloque dans recv : la politique temps réel ne consomme que le temps de décodage.
    if sink.is_some() {
        if let Err(kr) = lw_sys::rt::promote_for_packet_interval(Duration::from_millis(1)) {
            crate::error!("réception {group} : temps réel refusé (kern_return {kr})");
        }
    }
    let mut stats = RxStats::new(group);
    let mut buf = [0u8; 2048];
    let mut scratch = Vec::with_capacity(512);
    let mut next = Instant::now() + every;
    while !stop.requested() {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let sender = match from.ip() {
                    std::net::IpAddr::V4(v4) => v4,
                    std::net::IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
                };
                let accepted = stats.record(
                    Instant::now(),
                    sender,
                    buf.get(..n).unwrap_or_default(),
                    &mut scratch,
                );
                if let (true, Some(w)) = (accepted, sink.as_mut()) {
                    if !scratch.is_empty() && scratch.len() % w.channels() == 0 {
                        floats.clear();
                        floats.extend(scratch.iter().map(|&s| s as f32 / 8_388_608.0));
                        w.push(&floats);
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
        if Instant::now() >= next {
            stats.take_peak();
            on_report(&stats);
            next += every;
        }
    }
    // Récapitulatif : ne pas écraser la dernière crête mesurée s'il n'est rien arrivé depuis.
    if stats.peak > 0 || stats.peak_dbfs == f64::NEG_INFINITY {
        stats.take_peak();
    }
    Ok(stats)
}
