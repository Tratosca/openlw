//! Livewire / AES67 RTP reception and statistics: packets, loss, sequence errors,
//! jitter (RFC 3550).

use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use lw_proto::rtp::{decode_l24, Packet};
use serde::Serialize;

use crate::iface::Iface;
use crate::net::rx_socket;
use crate::Stop;

/// Cumulative statistics for a received stream.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RxStats {
    pub group: String,
    pub packets: u64,
    pub bytes: u64,
    pub invalid: u64,
    /// Missing packets (forward sequence jumps).
    pub lost: u64,
    /// Duplicates or late packets (difference in [-399, 0]).
    pub late_or_dup: u64,
    /// Out-of-window sequence breaks (new stream, transmitter restart).
    pub resyncs: u64,
    pub payload_types: BTreeMap<u8, u64>,
    pub payload_sizes: BTreeMap<usize, u64>,
    pub ts_steps: BTreeMap<u32, u64>,
    pub ssrc: Option<u32>,
    pub ssrc_is_group: bool,
    /// RFC 3550 jitter in 48 kHz samples.
    pub jitter_samples: f64,
    /// Peak since last report, dBFS (assuming L24).
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

    /// Process a packet received at `now`. Return `true` if accepted (not invalid,
    /// duplicate, or late); its L24 samples are then left in `scratch`.
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

    /// Snapshot the current peak in dBFS and reset it (periodic call).
    pub fn take_peak(&mut self) {
        self.peak_dbfs = if self.peak == 0 {
            f64::NEG_INFINITY
        } else {
            20.0 * (f64::from(self.peak) / 8_388_607.0).log10()
        };
        self.peak = 0;
    }
}

/// Receive `group:port` until stopped; call `on_report` approximately every `every`.
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

/// Like [`run`], also push decoded audio (L24 → float) into `sink` (device inputs).
/// Ignore packets whose size does not match the bus channel count.
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
    // Stream patched to device: reception delayed by tens of ms (normal-priority
    // thread) empties then overflows the jitter buffer (underrun, then slip).
    // The thread blocks in recv: real-time policy accounts only for decoding time.
    if sink.is_some() {
        if let Err(kr) = lw_sys::rt::promote_for_packet_interval(Duration::from_millis(1)) {
            crate::error!("receive {group}: real-time scheduling refused (code {kr})");
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
    // Summary: retain the last measured peak if nothing has arrived since.
    if stats.peak > 0 || stats.peak_dbfs == f64::NEG_INFINITY {
        stats.take_peak();
    }
    Ok(stats)
}
