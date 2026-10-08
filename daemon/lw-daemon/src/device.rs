//! Daemon side of the virtual device: region shared with the audio client (HAL plugin on macOS,
//! audio driver on Windows, daemon PipeWire nodes on Linux).
//!
//! One ring per device and direction: one of each in duplex layout, one per numbered device
//! in `multi` layout (output rings first, see `lw_shm.h`).
//!
//! A real-time thread running every 1 ms:
//! - publishes the clock in the region (host clock, ratio 1.0 until network synchronization);
//! - consumes each applications → network ring, measures per-channel peaks and feeds
//!   transmitted streams (routing table);
//! - fills each network → applications ring from received streams (stereo, surround, or one
//!   channel with a mono mix);
//! - in `loopback` test mode, copies output ring n to input ring n instead.

use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use lw_sys::shm::{Dir, Region, RingSpec};
use serde::{Deserialize, Serialize};

use crate::bus::{BusCounters, BusWriter, JitterReader};
use crate::Stop;

/// Virtual-device parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceConfig {
    /// Application output channels (to the network).
    #[serde(default = "default_channels")]
    pub channels_to_net: u32,
    /// Application input channels (from the network).
    #[serde(default = "default_channels")]
    pub channels_from_net: u32,
    /// Ring size in frames (power of two); 8192 = 170 ms at 48 kHz.
    #[serde(default = "default_ring")]
    pub ring_frames: u32,
    /// Copy application output to input (internal loopback, test mode).
    /// Test mode: takes precedence over patched inputs.
    #[serde(default)]
    pub loopback: bool,
}

fn default_channels() -> u32 {
    2
}
fn default_ring() -> u32 {
    8192
}

impl DeviceConfig {
    /// Duplex-layout rings: one output ring, one input ring.
    pub fn duplex_rings(&self) -> Vec<RingSpec> {
        vec![
            RingSpec::to_net(self.channels_to_net),
            RingSpec::from_net(self.channels_from_net),
        ]
    }
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            channels_to_net: 2,
            channels_from_net: 2,
            ring_frames: 8192,
            loopback: false,
        }
    }
}

/// Device state exposed through XPC.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceStatus {
    /// Shared-region number: changes on recreation (plugin must reattach).
    pub generation: u64,
    /// Device channels per direction (all devices).
    pub channels_to_net: u32,
    pub channels_from_net: u32,
    /// Channels of each output device and each input device (one entry each in duplex layout).
    pub out_widths: Vec<u32>,
    pub in_widths: Vec<u32>,
    pub ring_frames: u32,
    pub loopback: bool,
    /// Sample position published in the shared clock.
    pub clock_sample_time: u64,
    pub to_net_frames: u64,
    pub to_net_overruns: u64,
    pub from_net_frames: u64,
    pub from_net_overruns: u64,
    pub from_net_underruns: u64,
    /// Per-channel application audio peak over the last `METER_WINDOW` periods (dBFS), devices
    /// concatenated; refreshed every `METER_PERIOD`.
    pub to_net_peak_dbfs: Vec<f64>,
    /// Per-device-input peak (network audio), same window, devices concatenated.
    pub from_net_peak_dbfs: Vec<f64>,
    /// Active routes: device channels (1-based, devices concatenated) and bus counters.
    pub outputs: Vec<RouteStatus>,
    pub inputs: Vec<RouteStatus>,
}

/// Route state.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RouteStatus {
    pub label: String,
    /// Device channels numbered from 1 (as in configuration).
    pub device_channels: Vec<usize>,
    /// Inputs: jitter buffer primed (audio active).
    pub primed: bool,
    pub bus: BusCounters,
}

/// Device outputs to a transmitted stream.
pub struct OutRoute {
    pub label: String,
    /// Output ring (0 in duplex layout, device number − 1 in `multi` layout).
    pub ring: usize,
    /// Ring channels (0-based), in stream channel order.
    pub device_channels: Vec<usize>,
    pub writer: BusWriter,
}

/// Crosspoint of a received stream: one input of one ring.
pub struct RouteTap {
    /// Input ring (0 in duplex layout, device number − 1 in `multi` layout).
    pub ring: usize,
    /// Ring channel (0-based).
    pub channel: usize,
    /// Stream channels (0-based), averaged: one, or two for (L + R) / 2.
    pub from: Vec<usize>,
}

impl RouteTap {
    /// Value of this input for one stream frame (missing channels read as silence).
    fn value(&self, frame: &[f32]) -> f32 {
        let sum: f32 = self
            .from
            .iter()
            .map(|&c| frame.get(c).copied().unwrap_or(0.0))
            .sum();
        match self.from.len() {
            0 | 1 => sum,
            n => sum / n as f32,
        }
    }
}

/// Received stream to device inputs.
pub struct InRoute {
    pub label: String,
    pub taps: Vec<RouteTap>,
    pub reader: JitterReader,
    /// Frames pulled this tick (stream width × ring frames, allocated with the route).
    pub buffer: Vec<f32>,
}

impl InRoute {
    /// Route for `taps`; `ring_frames`: device ring size (largest pull per tick).
    pub fn new(
        label: String,
        taps: Vec<RouteTap>,
        reader: JitterReader,
        ring_frames: usize,
    ) -> Self {
        let buffer = vec![0f32; reader.channels() * ring_frames];
        Self {
            label,
            taps,
            reader,
            buffer,
        }
    }
}

/// Routing table, replaced live.
#[derive(Default)]
pub struct Routes {
    pub outputs: Vec<OutRoute>,
    pub inputs: Vec<InRoute>,
}

/// Running device.
pub struct Device {
    pub region: Arc<Region>,
    pub status: Arc<Mutex<DeviceStatus>>,
    pub thread: std::thread::JoinHandle<()>,
    pending: Arc<Mutex<Option<Routes>>>,
}

const SAMPLE_RATE: u32 = 48_000;
/// Output-ring read backlog recoverable at once (frames): absorbs host scheduling
/// without forwarding host blocks in bursts to transmitted streams.
const OUT_CARRY_MAX: usize = 512;
const TICK: Duration = Duration::from_millis(1);
/// Peak publication period (ticks), and window (periods) over which published peaks are held:
/// an app polling at most every 50 ms sees every peak, at most 10 ms late.
const METER_PERIOD: u32 = 10;
const METER_WINDOW: usize = 5;

fn dbfs(p: f32) -> f64 {
    if p > 0.0 {
        20.0 * f64::from(p).log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Per-channel peaks (linear) of the current period and of the last `METER_WINDOW` periods.
struct PeakWindow {
    acc: Vec<f32>,
    periods: Vec<f32>,
    next: usize,
}

impl PeakWindow {
    fn new(channels: usize) -> Self {
        Self {
            acc: vec![0.0; channels],
            periods: vec![0.0; channels * METER_WINDOW],
            next: 0,
        }
    }

    /// Close the current period and write the window maximum per channel (dBFS) to `out`.
    fn roll(&mut self, out: &mut Vec<f64>) {
        let c = self.acc.len();
        if let Some(slot) = self.periods.get_mut(self.next * c..(self.next + 1) * c) {
            slot.copy_from_slice(&self.acc);
        }
        self.acc.iter_mut().for_each(|p| *p = 0.0);
        self.next = (self.next + 1) % METER_WINDOW;
        out.clear();
        out.extend((0..c).map(|i| {
            dbfs(
                self.periods
                    .iter()
                    .skip(i)
                    .step_by(c)
                    .fold(0f32, |m, &p| m.max(p)),
            )
        }));
    }
}

/// Peak metering of both directions; `due`: publication postponed (status locked).
struct Meters {
    to_net: PeakWindow,
    from_net: PeakWindow,
    due: bool,
}

/// Ring channel count and offset in the concatenated (metering) channel space.
#[derive(Clone, Copy)]
struct Lane {
    ring: usize,
    channels: usize,
    offset: usize,
}

fn lanes(rings: &[RingSpec], dir: Dir) -> Vec<Lane> {
    let mut offset = 0;
    rings
        .iter()
        .enumerate()
        .filter(|(_, r)| r.dir == dir)
        .map(|(ring, r)| {
            let l = Lane {
                ring,
                channels: r.channels as usize,
                offset,
            };
            offset += l.channels;
            l
        })
        .collect()
}

/// Create the region and start the device thread. `rings`: output rings then input rings
/// ([`crate::config::Config::rings`]); duplex layout: one of each.
pub fn start(
    cfg: &DeviceConfig,
    rings: &[RingSpec],
    stop: &Stop,
) -> Result<Device, lw_sys::shm::Error> {
    let region = Arc::new(Region::create(SAMPLE_RATE, cfg.ring_frames, rings)?);
    let mut clock = region.clock_writer()?;
    let (out_lanes, in_lanes) = (lanes(rings, Dir::ToNet), lanes(rings, Dir::FromNet));
    let mut from_apps = Vec::with_capacity(out_lanes.len());
    for l in &out_lanes {
        from_apps.push(region.consumer(l.ring)?);
    }
    let mut to_apps = Vec::with_capacity(in_lanes.len());
    for l in &in_lanes {
        to_apps.push(region.producer(l.ring)?);
    }
    let out_widths: Vec<u32> = out_lanes.iter().map(|l| l.channels as u32).collect();
    let in_widths: Vec<u32> = in_lanes.iter().map(|l| l.channels as u32).collect();
    let (ch_in, ch_out) = (
        out_widths.iter().sum::<u32>() as usize,
        in_widths.iter().sum::<u32>() as usize,
    );
    let status = Arc::new(Mutex::new(DeviceStatus {
        channels_to_net: ch_in as u32,
        channels_from_net: ch_out as u32,
        out_widths,
        in_widths,
        ring_frames: cfg.ring_frames,
        loopback: cfg.loopback,
        to_net_peak_dbfs: vec![f64::NEG_INFINITY; ch_in],
        ..DeviceStatus::default()
    }));
    let pending: Arc<Mutex<Option<Routes>>> = Arc::new(Mutex::new(None));
    let (stop, st, reg, loopback, slot) = (
        stop.clone(),
        status.clone(),
        region.clone(),
        cfg.loopback,
        pending.clone(),
    );
    let thread = std::thread::Builder::new()
        .name("lw-device".into())
        .spawn(move || {
            if let Err(kr) = lw_sys::rt::promote_for_packet_interval(TICK) {
                crate::error!("device: real-time scheduling refused (code {kr})");
            }
            let ring = reg.geometry().ring_frames as usize;
            let widest = |l: &[Lane]| l.iter().map(|l| l.channels).max().unwrap_or(0);
            let (w_in, w_out) = (widest(&out_lanes), widest(&in_lanes));
            let t0_host = lw_sys::rt::host_time();
            let t0_ns = lw_sys::rt::host_time_ns();
            // Preallocated buffers, sized for the widest ring: no allocations in the loop.
            let mut buf = vec![0f32; w_in * ring];
            let mut out = vec![0f32; w_out * ring];
            let mut scratch = vec![0f32; 8 * ring];
            let mut meters = Meters {
                to_net: PeakWindow::new(ch_in),
                from_net: PeakWindow::new(ch_out),
                due: false,
            };
            let mut routes = Routes::default();
            let mut produced: u64 = 0;
            let mut out_clock: u64 = 0;
            // Unserved remainder per output ring.
            let mut owed = vec![0usize; out_lanes.len()];
            let start = Instant::now();
            let mut k: u32 = 0;
            clock.publish(t0_host, 0, 1.0);
            while !stop.requested() {
                k = k.wrapping_add(1);
                let deadline = start + TICK * k;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    lw_sys::rt::sleep(left);
                }
                // New routing table (nonblocking: one attempt per tick).
                if let Ok(mut s) = slot.try_lock() {
                    if let Some(r) = s.take() {
                        routes = r;
                    }
                }
                // Free-running host clock: position = elapsed time × 48 kHz.
                let now_host = lw_sys::rt::host_time();
                let elapsed_ns = lw_sys::rt::host_time_ns().saturating_sub(t0_ns);
                let sample = elapsed_ns * u64::from(SAMPLE_RATE) / 1_000_000_000;
                if k % 10 == 0 {
                    clock.publish(now_host, sample, 1.0);
                }
                // 1. Application audio, ring by ring: peaks, patched outputs, internal loopback.
                // Read at the clock rate (due frames, backlog carried up to OUT_CARRY_MAX):
                // the host writes blocks (4096 frames or more), smoothed by the shared ring;
                // forwarding them at once would overflow then empty transmit buffers.
                // The cap applies only to unserved remainder: a late wakeup of this thread
                // (coarse VM timers) catches up all due frames.
                let elapsed = sample.saturating_sub(out_clock) as usize;
                out_clock = sample;
                for (j, (lane, ring_in)) in out_lanes.iter().zip(from_apps.iter_mut()).enumerate() {
                    let Some(owe) = owed.get_mut(j) else {
                        continue;
                    };
                    let w = lane.channels;
                    let want = *owe + elapsed;
                    let n = want.min(ring_in.readable() as usize).min(ring);
                    *owe = (want - n).min(OUT_CARRY_MAX);
                    let (true, Some(block)) = (n > 0 && w > 0, buf.get_mut(..n * w)) else {
                        continue;
                    };
                    let _ = ring_in.read(block);
                    if let Some(p) = meters.to_net.acc.get_mut(lane.offset..lane.offset + w) {
                        for frame in block.chunks_exact(w) {
                            for (p, s) in p.iter_mut().zip(frame) {
                                *p = p.max(s.abs());
                            }
                        }
                    }
                    for route in routes.outputs.iter_mut().filter(|r| r.ring == j) {
                        let sc = route.device_channels.len();
                        if let Some(dst) = scratch.get_mut(..n * sc) {
                            for (frame, o) in block.chunks_exact(w).zip(dst.chunks_exact_mut(sc)) {
                                for (v, &c) in o.iter_mut().zip(&route.device_channels) {
                                    *v = frame.get(c).copied().unwrap_or(0.0);
                                }
                            }
                            route.writer.push(dst);
                        }
                    }
                    // Loopback: output ring j to input ring j (same channel index).
                    if let (true, Some(il), Some(ring_out)) =
                        (loopback, in_lanes.get(j), to_apps.get_mut(j))
                    {
                        let wo = il.channels;
                        if let (true, Some(dst)) = (wo > 0, out.get_mut(..n * wo)) {
                            for (frame, o) in block.chunks_exact(w).zip(dst.chunks_exact_mut(wo)) {
                                for (c, v) in o.iter_mut().enumerate() {
                                    *v = frame.get(c).copied().unwrap_or(0.0);
                                }
                            }
                            let _ = ring_out.write(dst);
                        }
                    }
                }
                // 2. Application inputs, ring by ring: frames due according to the clock. Fill
                // each ring while space remains; latency is bounded by the plugin, which knows
                // the host block size (it discards excess before reading).
                let due = sample.saturating_sub(produced) as usize;
                produced = sample;
                if loopback || due == 0 {
                    continue_status(
                        k,
                        sample,
                        &reg,
                        &out_lanes,
                        &in_lanes,
                        &st,
                        &mut meters,
                        &routes,
                    );
                    continue;
                }
                let frames = due.min(ring);
                // Each stream once per tick, whatever the number of inputs it feeds.
                for route in &mut routes.inputs {
                    let sc = route.reader.channels();
                    if let Some(src) = route.buffer.get_mut(..frames * sc) {
                        route.reader.pull(src);
                    }
                }
                for (j, (lane, ring_out)) in in_lanes.iter().zip(to_apps.iter_mut()).enumerate() {
                    let w = lane.channels;
                    let fed = routes
                        .inputs
                        .iter()
                        .any(|r| r.taps.iter().any(|t| t.ring == j));
                    if w == 0 || !fed {
                        continue;
                    }
                    let lp = meters
                        .from_net
                        .acc
                        .get_mut(lane.offset..lane.offset + w)
                        .unwrap_or_default();
                    // Ring full: nobody is reading (plugin I/O stopped). Streams are still
                    // pulled (no accumulated delay) and peaks measured: the app displays
                    // received audio before any recording.
                    let writable = reg.writable(lane.ring) as usize >= frames;
                    let mut dst = if writable {
                        out.get_mut(..frames * w)
                    } else {
                        None
                    };
                    if let Some(d) = dst.as_deref_mut() {
                        d.iter_mut().for_each(|v| *v = 0.0);
                    }
                    for route in &routes.inputs {
                        let sc = route.reader.channels();
                        let Some(src) = route.buffer.get(..frames * sc) else {
                            continue;
                        };
                        for t in route.taps.iter().filter(|t| t.ring == j) {
                            for (f, frame) in src.chunks_exact(sc).enumerate() {
                                let v = t.value(frame);
                                if let Some(p) = lp.get_mut(t.channel) {
                                    *p = p.max(v.abs());
                                }
                                if let Some(slot) = dst
                                    .as_deref_mut()
                                    .and_then(|d| d.get_mut(f * w + t.channel))
                                {
                                    *slot = v;
                                }
                            }
                        }
                    }
                    if let Some(d) = dst {
                        let _ = ring_out.write(d);
                    }
                }
                continue_status(
                    k,
                    sample,
                    &reg,
                    &out_lanes,
                    &in_lanes,
                    &st,
                    &mut meters,
                    &routes,
                );
            }
        });
    let thread = thread.map_err(|_| lw_sys::shm::Error("cannot create the device thread"))?;
    Ok(Device {
        region,
        status,
        thread,
        pending,
    })
}

/// Every `METER_PERIOD` ticks: peaks; every 100 ticks: ring counters (summed per direction),
/// route state.
#[allow(clippy::too_many_arguments)]
fn continue_status(
    k: u32,
    sample: u64,
    reg: &Region,
    out_lanes: &[Lane],
    in_lanes: &[Lane],
    st: &Mutex<DeviceStatus>,
    meters: &mut Meters,
    routes: &Routes,
) {
    // Peaks: never wait for the status lock, retry on the next tick.
    meters.due |= k % METER_PERIOD == 0;
    if meters.due {
        let s = match st.try_lock() {
            Ok(s) => Some(s),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut s) = s {
            meters.to_net.roll(&mut s.to_net_peak_dbfs);
            meters.from_net.roll(&mut s.from_net_peak_dbfs);
            meters.due = false;
        }
    }
    if k % 100 != 0 {
        return;
    }
    let mut s = st.lock().unwrap_or_else(PoisonError::into_inner);
    s.clock_sample_time = sample;
    let sum = |lanes: &[Lane]| {
        lanes.iter().fold((0u64, 0u64, 0u64), |acc, l| {
            let c = reg.counters(l.ring);
            (acc.0 + c.write_pos, acc.1 + c.overruns, acc.2 + c.underruns)
        })
    };
    let (a, b) = (sum(out_lanes), sum(in_lanes));
    s.to_net_frames = a.0;
    s.to_net_overruns = a.1;
    s.from_net_frames = b.0;
    s.from_net_overruns = b.1;
    s.from_net_underruns = b.2;
    // Route state: device channels reported in the concatenated space (1-based).
    let flat = |lanes: &[Lane], ring: usize, chs: &[usize]| -> Vec<usize> {
        let off = lanes.get(ring).map_or(0, |l| l.offset);
        chs.iter().map(|c| off + c + 1).collect()
    };
    s.outputs.truncate(routes.outputs.len());
    s.inputs.truncate(routes.inputs.len());
    for (i, r) in routes.outputs.iter().enumerate() {
        match s.outputs.get_mut(i) {
            Some(e) => e.bus = r.writer.counters(),
            None => s.outputs.push(RouteStatus {
                label: r.label.clone(),
                device_channels: flat(out_lanes, r.ring, &r.device_channels),
                primed: true,
                bus: r.writer.counters(),
            }),
        }
    }
    for (i, r) in routes.inputs.iter().enumerate() {
        match s.inputs.get_mut(i) {
            Some(e) => {
                e.bus = r.reader.counters();
                e.primed = r.reader.primed();
            }
            None => s.inputs.push(RouteStatus {
                label: r.label.clone(),
                device_channels: r
                    .taps
                    .iter()
                    .flat_map(|t| flat(in_lanes, t.ring, &[t.channel]))
                    .collect(),
                primed: r.reader.primed(),
                bus: r.reader.counters(),
            }),
        }
    }
}

impl Device {
    /// Replace the routing table; applied at the device thread's next tick.
    pub fn set_routes(&self, routes: Routes) {
        let mut st = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.outputs.clear();
        st.inputs.clear();
        drop(st);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(routes);
    }

    /// Access the route slot to pass it to a supervisor.
    pub fn routes_handle(&self) -> RoutesHandle {
        RoutesHandle {
            pending: self.pending.clone(),
            status: self.status.clone(),
        }
    }

    pub fn snapshot(&self) -> DeviceStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Cloneable handle for replacing routes on a running device.
#[derive(Clone)]
pub struct RoutesHandle {
    pending: Arc<Mutex<Option<Routes>>>,
    status: Arc<Mutex<DeviceStatus>>,
}

impl RoutesHandle {
    pub fn set(&self, routes: Routes) {
        let mut st = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.outputs.clear();
        st.inputs.clear();
        drop(st);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(routes);
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn peak_held_for_the_window() {
        let mut w = PeakWindow::new(2);
        let mut out = Vec::new();
        w.acc[0] = 0.5;
        w.roll(&mut out);
        assert!((out[0] - dbfs(0.5)).abs() < 1e-9);
        assert_eq!(out[1], f64::NEG_INFINITY);
        for _ in 1..METER_WINDOW {
            w.roll(&mut out);
            assert!((out[0] - dbfs(0.5)).abs() < 1e-9);
        }
        w.roll(&mut out);
        assert_eq!(out, [f64::NEG_INFINITY; 2]);
    }
}
