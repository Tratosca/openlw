//! Daemon ↔ audio client shared region: safe wrappers around `csrc/lw_shm.c`.
//!
//! Layout and logic (SPSC rings, clock seqlock) exist only in C: HAL plugin
//! and Windows driver compile the same file. Sharing: `xpc_shmem` object (macOS), section
//! duplicated into client process (Windows); on Linux, region remains in the daemon, which serves
//! PipeWire nodes itself. Rust guarantees one producer and one consumer
//! per ring per process: endpoints ([`Producer`], [`Consumer`], [`ClockWriter`]) cannot be
//! cloned and can be obtained only once.

use std::ffi::c_void;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ffi;

/// Ring direction. In a duplex region (two rings) it is also the ring index: `Dir` converts
/// into `usize` for ring-indexed methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Applications → network (producer: plugin; consumer: daemon).
    ToNet = 0,
    /// Network → applications (producer: daemon; consumer: plugin).
    FromNet = 1,
}

impl From<Dir> for usize {
    fn from(d: Dir) -> usize {
        d as usize
    }
}

/// Ring description: direction and channel count. Region rings are ordered TO_NET first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingSpec {
    /// Direction.
    pub dir: Dir,
    /// Interleaved frame width.
    pub channels: u32,
}

impl RingSpec {
    /// Applications → network ring.
    pub fn to_net(channels: u32) -> Self {
        Self {
            dir: Dir::ToNet,
            channels,
        }
    }

    /// Network → applications ring.
    pub fn from_net(channels: u32) -> Self {
        Self {
            dir: Dir::FromNet,
            channels,
        }
    }

    fn ffi(self) -> ffi::RingSpec {
        ffi::RingSpec {
            dir: self.dir as u32,
            channels: self.channels,
        }
    }
}

/// Shared-region error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

/// Server-provided sharing object: retained `xpc_shmem` (macOS) or section handle (Windows).
/// Freed on `drop` if unmapped.
pub struct SharedObject(*mut c_void);

// SAFETY: a retained XPC object or Windows handle can be transferred and released from any
// thread.
unsafe impl Send for SharedObject {}

impl SharedObject {
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn from_raw(p: *mut c_void) -> Option<Self> {
        (!p.is_null()).then_some(Self(p))
    }

    /// Section handle from daemon (`shmem.handle` in `attach` response), already
    /// duplicated into this process by daemon. Mapping validates region.
    #[cfg(windows)]
    pub fn from_handle_value(value: u64) -> Option<Self> {
        Self::from_raw(usize::try_from(value).ok()? as *mut c_void)
    }
}

impl Drop for SharedObject {
    fn drop(&mut self) {
        // SAFETY: retained object or handle owned by this value, released once.
        unsafe { ffi::lw_shm_release(self.0) };
    }
}

struct Inner {
    base: *mut c_void,
    size: usize,
    /// Creator sharing object or received client object, released on drop; NULL on Linux.
    handle: *mut c_void,
    /// Endpoints currently held (producer and consumer of each ring, then clock writer);
    /// released when the endpoint is dropped.
    taken: Vec<AtomicBool>,
}

impl Inner {
    fn release(&self, slot: usize) {
        if let Some(flag) = self.taken.get(slot) {
            flag.store(false, Ordering::Release);
        }
    }
}

// SAFETY: memory accessed only through C functions, atomic for positions; audio-data
// sharing serialized by SPSC protocol guaranteed by API (one endpoint per role).
unsafe impl Send for Inner {}
// SAFETY: likewise; read-only methods (`readable`, `counters`, `clock_read`) are atomic.
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: `base`/`size` come from successful mapping, unmapped once; `handle` is
        // owned by this region.
        unsafe {
            ffi::lw_shm_unmap(self.base, self.size);
            ffi::lw_shm_release(self.handle);
        }
    }
}

/// Mapped shared region. Each endpoint is held by at most one owner at a time, and becomes
/// available again once that owner drops it.
pub struct Region {
    inner: Arc<Inner>,
}

/// Parameters read from header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    /// Sample rate (Hz).
    pub sample_rate: u32,
    /// Ring size in frames (power of two), shared by all rings.
    pub ring_frames: u32,
    /// Ring table, in region order.
    pub rings: Vec<RingSpec>,
}

/// Ring counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Write position (frames, increasing).
    pub write_pos: u64,
    /// Read position (frames, increasing).
    pub read_pos: u64,
    /// Frames rejected due to insufficient space.
    pub overruns: u64,
    /// Missing frames padded with silence.
    pub underruns: u64,
}

impl Region {
    /// Create a duplex region (daemon side): TO_NET ring (index 0) and FROM_NET ring (index 1).
    pub fn create_duplex(
        sample_rate: u32,
        ring_frames: u32,
        channels_to_net: u32,
        channels_from_net: u32,
    ) -> Result<Self, Error> {
        Self::create(
            sample_rate,
            ring_frames,
            &[
                RingSpec::to_net(channels_to_net),
                RingSpec::from_net(channels_from_net),
            ],
        )
    }

    /// Create region (daemon side): rings of `ring_frames` frames (power of two), TO_NET
    /// rings first.
    pub fn create(sample_rate: u32, ring_frames: u32, rings: &[RingSpec]) -> Result<Self, Error> {
        let specs: Vec<ffi::RingSpec> = rings.iter().map(|r| r.ffi()).collect();
        let n = u32::try_from(specs.len()).map_err(|_| Error("too many rings"))?;
        // SAFETY: pure C function; `specs` holds `n` entries.
        let size = unsafe { ffi::lw_shm_size(ring_frames, n, specs.as_ptr()) };
        if size == 0 {
            return Err(Error(
                "invalid geometry (ring a power of two between 64 and 65536, 1 to 32 rings, TO_NET first, ≤ 64 channels)",
            ));
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: `handle` is a valid output pointer; C layer allocates `size` bytes.
        let base = unsafe { ffi::lw_shm_alloc(size, &mut handle) };
        if base.is_null() {
            return Err(Error("cannot allocate the shared region"));
        }
        let mut clock = ffi::HostClock::default();
        // SAFETY: valid output pointer.
        unsafe { ffi::lw_host_clock_info(&mut clock) };
        // SAFETY: `base` points to `size` freshly mapped bytes, not yet shared.
        let rc = unsafe {
            ffi::lw_shm_init(
                base,
                size,
                sample_rate,
                ring_frames,
                n,
                specs.as_ptr(),
                &clock,
            )
        };
        let region = Self::wrap(base, size, handle, rings.len());
        if rc != 0 {
            return Err(Error("cannot initialize the region"));
        }
        Ok(region)
    }

    /// Map and validate region received from daemon (client side or test).
    pub fn map(object: SharedObject) -> Result<Self, Error> {
        let mut size = 0usize;
        // SAFETY: `object.0` is an owned sharing object; `size` is a valid output pointer.
        let base = unsafe { ffi::lw_shm_map(object.0, &mut size) };
        if base.is_null() {
            return Err(Error("cannot map the region"));
        }
        let obj = object.0;
        std::mem::forget(object); // Object ownership transfers to `Inner`
                                  // SAFETY: `base` points to `size` mapped bytes.
        let valid = unsafe { ffi::lw_shm_validate(base, size) } == 0;
        let rings = if valid {
            // SAFETY: validated header.
            unsafe { ffi::lw_shm_ring_count(base) }
        } else {
            0
        };
        let region = Self::wrap(base, size, obj, rings as usize);
        if !valid {
            return Err(Error("invalid region (magic, version or size)"));
        }
        Ok(region)
    }

    fn wrap(base: *mut c_void, size: usize, handle: *mut c_void, rings: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                base,
                size,
                handle,
                taken: (0..=rings * 2).map(|_| AtomicBool::new(false)).collect(),
            }),
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn handle(&self) -> *mut c_void {
        self.inner.handle
    }

    /// Duplicate section into process `pid`; return handle value in that process.
    #[cfg(windows)]
    pub fn share_with(&self, pid: u32) -> Option<u64> {
        // SAFETY: `handle` is this region's section, valid for its lifetime.
        let h = unsafe { ffi::lw_shm_share_with(self.inner.handle, pid) };
        (h != 0).then_some(h)
    }

    /// Header-declared host clock: (`lw_host_clock_id` identifier, numerator,
    /// denominator for conversion to ns).
    pub fn host_clock(&self) -> (u32, u64, u64) {
        let mut c = ffi::HostClock::default();
        // SAFETY: valid region; valid output pointer.
        unsafe { ffi::lw_shm_host_clock(self.inner.base, &mut c) };
        (c.id, c.ns_numer, c.ns_denom)
    }

    /// Region size in bytes.
    pub fn size(&self) -> usize {
        self.inner.size
    }

    /// Geometry read from header (fields immutable after initialization).
    pub fn geometry(&self) -> Geometry {
        let b = self.inner.base;
        // SAFETY: validated region; accessors bound-check the ring index.
        unsafe {
            Geometry {
                sample_rate: ffi::lw_shm_sample_rate(b),
                ring_frames: ffi::lw_shm_ring_frames(b),
                rings: (0..ffi::lw_shm_ring_count(b))
                    .map(|i| RingSpec {
                        dir: if ffi::lw_ring_dir(b, i) == 0 {
                            Dir::ToNet
                        } else {
                            Dir::FromNet
                        },
                        channels: ffi::lw_ring_channels(b, i),
                    })
                    .collect(),
            }
        }
    }

    /// Ring count.
    pub fn ring_count(&self) -> usize {
        // SAFETY: validated region.
        unsafe { ffi::lw_shm_ring_count(self.inner.base) as usize }
    }

    /// Channel count for ring `ring` (0 if out of range).
    pub fn channels(&self, ring: impl Into<usize>) -> u32 {
        // SAFETY: validated region; accessor bound-checks.
        unsafe { ffi::lw_ring_channels(self.inner.base, ring_id(ring)) }
    }

    /// Readable frames in ring `ring`.
    pub fn readable(&self, ring: impl Into<usize>) -> u32 {
        // SAFETY: valid region; atomic read; index bound-checked in C.
        unsafe { ffi::lw_ring_readable(self.inner.base, ring_id(ring)) }
    }

    /// Free space (frames) in ring `ring`.
    pub fn writable(&self, ring: impl Into<usize>) -> u32 {
        // SAFETY: valid region; atomic read; index bound-checked in C.
        unsafe { ffi::lw_ring_writable(self.inner.base, ring_id(ring)) }
    }

    /// Ring `ring` positions and counters.
    pub fn counters(&self, ring: impl Into<usize>) -> Counters {
        let mut c = Counters::default();
        // SAFETY: valid region; valid output pointers.
        unsafe {
            ffi::lw_ring_counters(
                self.inner.base,
                ring_id(ring),
                &mut c.write_pos,
                &mut c.read_pos,
                &mut c.overruns,
                &mut c.underruns,
            );
        }
        c
    }

    /// Coherent published-clock read: (raw host time, sample position, ratio).
    pub fn clock(&self) -> Option<(u64, u64, f64)> {
        let (mut h, mut s, mut r) = (0u64, 0u64, 0f64);
        // SAFETY: valid region; valid output pointers.
        let rc = unsafe { ffi::lw_clock_read(self.inner.base, &mut h, &mut s, &mut r) };
        (rc == 0).then_some((h, s, r))
    }

    fn take(&self, slot: usize) -> Result<(), Error> {
        let flag = self
            .inner
            .taken
            .get(slot)
            .ok_or(Error("unknown endpoint"))?;
        if flag.swap(true, Ordering::AcqRel) {
            Err(Error("endpoint already taken"))
        } else {
            Ok(())
        }
    }

    fn ring_index(&self, ring: impl Into<usize>) -> Result<usize, Error> {
        let ring = ring.into();
        if ring < self.ring_count() {
            Ok(ring)
        } else {
            Err(Error("unknown ring"))
        }
    }

    /// Producer for ring `ring` (one owner at a time).
    pub fn producer(&self, ring: impl Into<usize>) -> Result<Producer, Error> {
        let ring = self.ring_index(ring)?;
        let slot = ring * 2;
        self.take(slot)?;
        Ok(Producer {
            inner: self.inner.clone(),
            ring: ring_id(ring),
            channels: self.channels(ring),
            slot,
        })
    }

    /// Consumer for ring `ring` (one owner at a time).
    pub fn consumer(&self, ring: impl Into<usize>) -> Result<Consumer, Error> {
        let ring = self.ring_index(ring)?;
        let slot = ring * 2 + 1;
        self.take(slot)?;
        Ok(Consumer {
            inner: self.inner.clone(),
            ring: ring_id(ring),
            channels: self.channels(ring),
            slot,
        })
    }

    /// Clock writer (daemon, one owner at a time).
    pub fn clock_writer(&self) -> Result<ClockWriter, Error> {
        let slot = self.inner.taken.len() - 1;
        self.take(slot)?;
        Ok(ClockWriter {
            inner: self.inner.clone(),
            slot,
        })
    }
}

/// C ring index; values beyond `u32` map to an out-of-range ring (inert in C).
fn ring_id(ring: impl Into<usize>) -> u32 {
    u32::try_from(ring.into()).unwrap_or(u32::MAX)
}

fn frames_of(len: usize, channels: u32) -> Result<u32, Error> {
    if channels == 0 {
        return Ok(0);
    }
    let ch = channels as usize;
    if len % ch != 0 {
        return Err(Error("length not a multiple of the channel count"));
    }
    u32::try_from(len / ch).map_err(|_| Error("too many frames"))
}

/// Ring producer.
pub struct Producer {
    inner: Arc<Inner>,
    ring: u32,
    channels: u32,
    slot: usize,
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.inner.release(self.slot);
    }
}

impl Producer {
    /// Channels per frame.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Write interleaved frames; return accepted count (remainder counts as overruns).
    pub fn write(&mut self, interleaved: &[f32]) -> Result<u32, Error> {
        let frames = frames_of(interleaved.len(), self.channels)?;
        // SAFETY: unique producer for this ring (guaranteed by `Region::producer`); `interleaved`
        // contains `frames × channels` samples.
        Ok(unsafe { ffi::lw_ring_write(self.inner.base, self.ring, interleaved.as_ptr(), frames) })
    }
}

/// Ring consumer.
pub struct Consumer {
    inner: Arc<Inner>,
    ring: u32,
    channels: u32,
    slot: usize,
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.inner.release(self.slot);
    }
}

impl Consumer {
    /// Channels per frame.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Readable frames.
    pub fn readable(&self) -> u32 {
        // SAFETY: valid region; atomic read.
        unsafe { ffi::lw_ring_readable(self.inner.base, self.ring) }
    }

    /// Discard up to `frames` oldest frames (latency catch-up); return discarded
    /// count.
    pub fn skip(&mut self, frames: u32) -> u32 {
        // SAFETY: unique consumer for this ring (guaranteed by `Region::consumer`).
        unsafe { ffi::lw_ring_skip(self.inner.base, self.ring, frames) }
    }

    /// Fill `out` (interleaved frames); pad missing frames with silence.
    /// Return actual frame count read.
    pub fn read(&mut self, out: &mut [f32]) -> Result<u32, Error> {
        let frames = frames_of(out.len(), self.channels)?;
        // SAFETY: unique consumer for this ring; `out` can hold `frames × channels` samples.
        Ok(unsafe { ffi::lw_ring_read(self.inner.base, self.ring, out.as_mut_ptr(), frames) })
    }
}

/// Shared-clock writer.
pub struct ClockWriter {
    inner: Arc<Inner>,
    slot: usize,
}

impl Drop for ClockWriter {
    fn drop(&mut self) {
        self.inner.release(self.slot);
    }
}

impl ClockWriter {
    /// Publish: at raw host instant `host_time` ([`crate::rt::host_time`]), position is `sample_time`.
    pub fn publish(&mut self, host_time: u64, sample_time: u64, rate_scalar: f64) {
        // SAFETY: unique writer (guaranteed by `Region::clock_writer`).
        unsafe { ffi::lw_clock_publish(self.inner.base, host_time, sample_time, rate_scalar) };
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn geometry_and_invalid_parameters() {
        let r = Region::create_duplex(48_000, 1024, 8, 2).unwrap();
        assert_eq!(
            r.geometry(),
            Geometry {
                sample_rate: 48_000,
                ring_frames: 1024,
                rings: vec![RingSpec::to_net(8), RingSpec::from_net(2)],
            }
        );
        assert_eq!(r.size(), 8192 + 1024 * 10 * 4);
        assert!(
            Region::create_duplex(48_000, 1000, 2, 2).is_err(),
            "ring not a power of two"
        );
        assert!(
            Region::create_duplex(48_000, 1024, 65, 2).is_err(),
            "too many channels"
        );
        assert!(
            Region::create(48_000, 1024, &[RingSpec::from_net(2), RingSpec::to_net(2)]).is_err(),
            "TO_NET rings come first"
        );
        assert!(Region::create(48_000, 1024, &[]).is_err(), "no ring");
        let many = vec![RingSpec::from_net(2); 33];
        assert!(
            Region::create(48_000, 1024, &many).is_err(),
            "too many rings"
        );
    }

    #[test]
    fn rings_are_independent() {
        // Multi-device layout: Out 1 (2 ch), In 1 (1 ch), In 2 (8 ch).
        let rings = [
            RingSpec::to_net(2),
            RingSpec::from_net(1),
            RingSpec::from_net(8),
        ];
        let r = Region::create(48_000, 256, &rings).unwrap();
        assert_eq!(r.size(), 8192 + 256 * 11 * 4);
        assert_eq!(r.geometry().rings, rings);
        assert_eq!(r.ring_count(), 3);
        assert_eq!(
            (r.channels(1usize), r.channels(2usize), r.channels(3usize)),
            (1, 8, 0)
        );
        let (mut p1, mut p2) = (r.producer(1usize).unwrap(), r.producer(2usize).unwrap());
        let (mut c1, mut c2) = (r.consumer(1usize).unwrap(), r.consumer(2usize).unwrap());
        assert!(r.producer(3usize).is_err(), "unknown ring");
        assert_eq!(p1.write(&[0.25; 10]).unwrap(), 10);
        assert_eq!(p2.write(&[-0.5; 8 * 4]).unwrap(), 4);
        assert_eq!(
            (
                r.readable(1usize),
                r.readable(2usize),
                r.readable(Dir::ToNet)
            ),
            (10, 4, 0)
        );
        let mut a = vec![0f32; 10];
        let mut b = vec![0f32; 8 * 4];
        assert_eq!(c1.read(&mut a).unwrap(), 10);
        assert_eq!(c2.read(&mut b).unwrap(), 4);
        assert!(
            a.iter().all(|&s| s == 0.25) && b.iter().all(|&s| s == -0.5),
            "no crosstalk"
        );
        assert_eq!(r.counters(2usize).read_pos, 4);
        let _w = r.clock_writer().unwrap();
        assert!(
            r.clock_writer().is_err(),
            "clock slot follows the ring slots"
        );
    }

    #[test]
    fn ends_are_unique_while_held() {
        let r = Region::create_duplex(48_000, 256, 2, 2).unwrap();
        let p = r.producer(Dir::ToNet).unwrap();
        assert!(r.producer(Dir::ToNet).is_err());
        let _c = r.consumer(Dir::ToNet).unwrap();
        let w = r.clock_writer().unwrap();
        assert!(r.clock_writer().is_err());
        drop(p);
        drop(w);
        assert!(r.producer(Dir::ToNet).is_ok(), "released on drop");
        assert!(r.clock_writer().is_ok());
    }

    #[test]
    fn ring_wraps_and_counts() {
        let r = Region::create_duplex(48_000, 64, 2, 2).unwrap();
        let (mut p, mut c) = (
            r.producer(Dir::FromNet).unwrap(),
            r.consumer(Dir::FromNet).unwrap(),
        );
        let mut out = vec![0f32; 2 * 40];
        let mut next = 0f32;
        for _ in 0..10 {
            let block: Vec<f32> = (0..80).map(|i| next + i as f32).collect();
            assert_eq!(p.write(&block).unwrap(), 40);
            assert_eq!(c.read(&mut out).unwrap(), 40);
            assert_eq!(out, block, "identical content after wrap-around");
            next += 80.0;
        }
        // Overrun: 64 frames free, 100 requested.
        let big = vec![1f32; 2 * 100];
        assert_eq!(p.write(&big).unwrap(), 64);
        assert_eq!(r.counters(Dir::FromNet).overruns, 36);
        // Underrun: 64 readable, 70 requested → six silent frames.
        let mut out = vec![9f32; 2 * 70];
        assert_eq!(c.read(&mut out).unwrap(), 64);
        assert_eq!(r.counters(Dir::FromNet).underruns, 6);
        assert!(out[128..].iter().all(|&s| s == 0.0));
        assert!(
            p.write(&[1.0]).is_err(),
            "length not a multiple of the channels"
        );
    }

    #[test]
    fn host_clock_is_declared() {
        let r = Region::create_duplex(48_000, 256, 2, 2).unwrap();
        let (id, numer, denom) = r.host_clock();
        let expected = if cfg!(target_os = "macos") {
            1
        } else if cfg!(windows) {
            2
        } else {
            3
        };
        assert_eq!(id, expected);
        assert!(numer > 0 && denom > 0);
    }

    /// Daemon creates, client maps sharing object: two addresses, same memory.
    #[cfg(target_os = "macos")]
    fn second_mapping(daemon: &Region) -> Region {
        let server =
            crate::xpc::Server::start(None, Box::new(|_, _| "{\"ok\":true}".into())).unwrap();
        server.set_shmem(daemon);
        let client = crate::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        let (_, obj) = client.call_with_shmem("{}").unwrap();
        Region::map(obj.expect("xpc_shmem object received")).unwrap()
    }

    #[cfg(windows)]
    fn second_mapping(daemon: &Region) -> Region {
        let h = daemon
            .share_with(std::process::id())
            .expect("section duplicated");
        Region::map(SharedObject::from_handle_value(h).unwrap()).unwrap()
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn spsc_across_two_mappings() {
        let daemon = Region::create_duplex(48_000, 1024, 2, 2).unwrap();
        let plugin = second_mapping(&daemon);
        assert_eq!(plugin.geometry(), daemon.geometry());
        assert_eq!(plugin.host_clock(), daemon.host_clock());

        let mut producer = plugin.producer(Dir::ToNet).unwrap();
        let mut consumer = daemon.consumer(Dir::ToNet).unwrap();
        const TOTAL: u32 = 200_000;
        let writer = std::thread::spawn(move || {
            let mut n = 0u32;
            while n < TOTAL {
                let k = 37.min(TOTAL - n);
                let block: Vec<f32> = (n..n + k).flat_map(|i| [i as f32, -(i as f32)]).collect();
                let w = producer.write(&block).unwrap();
                n += w;
                if w < k {
                    std::thread::yield_now();
                    // Rejected frames count as overrun: send subsequent data, not the remainder.
                    let lost = k - w;
                    n += lost;
                }
            }
        });
        let mut got = 0u64;
        let mut expect = 0f32;
        let mut buf = vec![0f32; 2 * 29];
        let mut idle = 0;
        while idle < 200_000 {
            let avail = consumer.readable().min(29);
            if avail == 0 {
                idle += 1;
                std::thread::yield_now();
                continue;
            }
            idle = 0;
            let slice = &mut buf[..2 * avail as usize];
            assert_eq!(consumer.read(slice).unwrap(), avail);
            for f in slice.chunks_exact(2) {
                assert!(f[0] >= expect, "order preserved");
                assert_eq!(f[1], -f[0], "frame intact (no tearing)");
                expect = f[0] + 1.0;
                got += 1;
            }
        }
        writer.join().unwrap();
        let c = daemon.counters(Dir::ToNet);
        assert_eq!(
            got + c.overruns,
            u64::from(TOTAL),
            "every frame is either read or counted as lost"
        );
    }

    #[test]
    fn clock_seqlock_is_consistent() {
        let r = Region::create_duplex(48_000, 256, 2, 2).unwrap();
        assert!(r.clock().is_none(), "no clock before publication");
        let mut w = r.clock_writer().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut i = 1u64;
            while !s2.load(Ordering::Relaxed) {
                w.publish(i, i * 2, i as f64 * 0.5);
                i += 1;
            }
        });
        let mut reads = 0;
        while reads < 200_000 {
            if let Some((h, s, rate)) = r.clock() {
                assert_eq!(s, h * 2, "consistent triple");
                assert_eq!(rate, h as f64 * 0.5);
                reads += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
}
