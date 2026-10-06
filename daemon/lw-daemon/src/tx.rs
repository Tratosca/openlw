//! Émission RTP d'un flux Livewire / AES67 (générateur de test en attendant le ring buffer du plugin).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use lw_proto::channel::{Channel, AUDIO_PORT};
use lw_proto::format::{StreamFormat, SAMPLE_RATE};
use lw_proto::rtp::{ssrc_from_group, Packetizer};
use serde::Serialize;

use crate::iface::Iface;
use crate::net::{mark_dscp, tx_socket, TxOptions};
use crate::Stop;

/// Générateur de sinusoïde, identique sur tous les canaux.
#[derive(Debug, Clone)]
pub struct Tone {
    pub freq_hz: f64,
    pub level_dbfs: f64,
    phase: u64,
}

impl Tone {
    pub fn new(freq_hz: f64, level_dbfs: f64) -> Self {
        Self {
            freq_hz,
            level_dbfs,
            phase: 0,
        }
    }

    /// Remplit `out` avec `frames` trames de `channels` échantillons 24 bits.
    pub fn fill(&mut self, frames: u32, channels: u16, out: &mut Vec<i32>) {
        out.clear();
        let amp = 8_388_607.0 * 10f64.powf(self.level_dbfs / 20.0);
        let w = 2.0 * std::f64::consts::PI * self.freq_hz / f64::from(SAMPLE_RATE);
        for _ in 0..frames {
            let s = (amp * (w * self.phase as f64).sin()).round() as i32;
            out.extend(std::iter::repeat_n(s, usize::from(channels)));
            self.phase += 1;
        }
    }
}

/// Description d'un flux émis.
#[derive(Debug, Clone)]
pub struct TxStream {
    pub channel: Channel,
    pub format: StreamFormat,
    pub payload_type: u8,
    pub port: u16,
    pub tone: Tone,
}

impl TxStream {
    pub fn new(channel: Channel, format: StreamFormat) -> Self {
        Self {
            channel,
            format,
            payload_type: 96,
            port: AUDIO_PORT,
            tone: Tone::new(997.0, -20.0),
        }
    }

    pub fn group(&self) -> Ipv4Addr {
        self.channel.group(self.format.group_kind())
    }
}

/// Bilan d'émission.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TxReport {
    pub group: String,
    pub packets: u64,
    pub send_errors: u64,
    /// Retard maximal d'un envoi par rapport à son échéance.
    pub max_late_us: u64,
    /// Paquets envoyés avec plus d'un intervalle de retard.
    pub late_packets: u64,
    /// Le thread a obtenu l'ordonnancement temps réel.
    pub realtime: bool,
}

/// Émet le flux jusqu'à l'arrêt, cadencé sur des échéances absolues (pas de dérive cumulée).
pub fn run(iface: &Iface, stream: &TxStream, opts: TxOptions, stop: &Stop) -> io::Result<TxReport> {
    run_with_progress(iface, stream, opts, stop, |_| {})
}

/// Comme [`run`], avec un bilan intermédiaire environ chaque seconde.
pub fn run_with_progress(
    iface: &Iface,
    stream: &TxStream,
    opts: TxOptions,
    stop: &Stop,
    on_progress: impl FnMut(&TxReport),
) -> io::Result<TxReport> {
    run_from(iface, stream, opts, stop, None, on_progress)
}

/// Émet le flux ; l'audio vient de `source` (sorties du périphérique) s'il est fourni, sinon du
/// générateur de test du flux. Échantillons float → L24 avec saturation.
pub fn run_from(
    iface: &Iface,
    stream: &TxStream,
    opts: TxOptions,
    stop: &Stop,
    mut source: Option<crate::bus::JitterReader>,
    mut on_progress: impl FnMut(&TxReport),
) -> io::Result<TxReport> {
    let mut floats = vec![
        0f32;
        stream.format.samples_per_packet() as usize
            * usize::from(stream.format.channels())
    ];
    let group = stream.group();
    let sock = tx_socket(iface, stream.port, opts)?;
    let dest = SocketAddrV4::new(group, stream.port);
    let _dscp = mark_dscp(&sock, dest, opts.tos)
        .map_err(|e| crate::error!("{group} : marquage DSCP refusé (erreur {e})"))
        .ok()
        .flatten();
    // Séquence et horodatage dérivés de l'horloge hôte : un flux relancé reprend là où un flux
    // continu serait (saut vers l'avant, vu comme une perte) au lieu de repartir de zéro.
    let spp = u64::from(stream.format.samples_per_packet()).max(1);
    let frames = lw_sys::rt::host_time_ns() / 1_000 * 48 / 1_000;
    let mut packetizer = Packetizer::new(
        stream.format,
        stream.payload_type,
        ssrc_from_group(group),
        (frames / spp) as u16,
        (frames / spp * spp) as u32,
    );
    let mut tone = stream.tone.clone();
    let interval = Duration::from_micros(u64::from(stream.format.packet_interval_us()));
    let mut report = TxReport {
        group: group.to_string(),
        ..TxReport::default()
    };
    if opts.realtime {
        match lw_sys::rt::promote_for_packet_interval(interval) {
            Ok(()) => report.realtime = true,
            Err(kr) => {
                crate::error!("{group} : ordonnancement temps réel refusé (code {kr})")
            }
        }
    }
    let (mut samples, mut pkt) = (Vec::new(), Vec::with_capacity(1500));
    let start = Instant::now();
    let mut next_progress = start + Duration::from_secs(1);
    let mut k: u32 = 0;
    while !stop.requested() {
        let deadline = start + interval * k;
        if report.realtime {
            wait_until_blocking(deadline);
        } else {
            wait_until(deadline);
        }
        let late = Instant::now().saturating_duration_since(deadline);
        report.max_late_us = report.max_late_us.max(late.as_micros() as u64);
        if late > interval {
            report.late_packets += 1;
        }
        match source.as_mut() {
            Some(src) => {
                src.pull(&mut floats);
                samples.clear();
                samples.extend(floats.iter().map(|&f| {
                    (f64::from(f) * 8_388_607.0)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0) as i32
                }));
            }
            None => tone.fill(
                stream.format.samples_per_packet(),
                stream.format.channels(),
                &mut samples,
            ),
        }
        packetizer
            .packet(&samples, &mut pkt)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        match sock.send_to(&pkt, dest) {
            Ok(_) => report.packets += 1,
            Err(_) => report.send_errors += 1,
        }
        k = k.wrapping_add(1);
        if Instant::now() >= next_progress {
            on_progress(&report);
            next_progress += Duration::from_secs(1);
        }
    }
    Ok(report)
}

/// Attente en thread temps réel : sommeil précis uniquement, jamais d'attente active
/// (un dépassement du budget de calcul fait rétrograder le thread par le noyau).
fn wait_until_blocking(deadline: Instant) {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        lw_sys::rt::sleep(left);
    }
}

/// Attente en thread normal : sommeil grossier puis attente active courte.
fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_micros(1500) {
            std::thread::sleep(left - Duration::from_micros(1000));
        } else {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn tone_level_and_layout() {
        let mut t = Tone::new(1000.0, -20.0);
        let mut v = Vec::new();
        t.fill(480, 2, &mut v);
        assert_eq!(v.len(), 960);
        let peak = v.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let db = 20.0 * (f64::from(peak) / 8_388_607.0).log10();
        assert!((db + 20.0).abs() < 0.1, "crête {db} dBFS");
        assert!(v.chunks_exact(2).all(|f| f.first() == f.get(1)));
    }
}
