//! Daemon ↔ audio client shared region: safe wrappers around `csrc/lw_shm.c`.
//!
//! Layout and logic (SPSC rings, clock seqlock) exist only in C: HAL plugin
//! and Windows driver compile the same file. Sharing: `xpc_shmem` object (macOS), section
//! duplicated into client process (Windows); on Linux, region remains in the daemon, which serves
//! PipeWire nodes itself. Rust guarantees one producer and one consumer
//! per ring per process: endpoints ([`Producer`], [`Consumer`], [`ClockWriter`]) cannot be
//! cloned and can be obtained only once.

use std::ffi::{c_int, c_void};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ffi;

/// Ring direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Applications → network (producer: plugin; consumer: daemon).
    ToNet = 0,
    /// Network → applications (producer: daemon; consumer: plugin).
    FromNet = 1,
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

/// Mapped shared region. Each endpoint obtainable once.
pub struct Region {
    inner: Arc<Inner>,
    taken: [AtomicBool; 5],
}

/// Parameters read from header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Sample rate (Hz).
    pub sample_rate: u32,
    /// Ring size in frames (power of two).
    pub ring_frames: u32,
    /// Applications → network ring channels.
    pub channels_to_net: u32,
    /// Network → applications ring channels.
    pub channels_from_net: u32,
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
    /// Create region (daemon side): rings of `ring_frames` frames (power of two).
    pub fn create(
        sample_rate: u32,
        ring_frames: u32,
        channels_to_net: u32,
        channels_from_net: u32,
    ) -> Result<Self, Error> {
        // SAFETY: pure C function.
        let size = unsafe { ffi::lw_shm_size(ring_frames, channels_to_net, channels_from_net) };
        if size == 0 {
            return Err(Error(
                "géométrie invalide (anneau puissance de 2 entre 64 et 65536, ≤ 64 canaux)",
            ));
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: `handle` is a valid output pointer; C layer allocates `size` bytes.
        let base = unsafe { ffi::lw_shm_alloc(size, &mut handle) };
        if base.is_null() {
            return Err(Error("allocation de la région partagée impossible"));
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
                channels_to_net,
                channels_from_net,
                &clock,
            )
        };
        let region = Self::wrap(base, size, handle);
        if rc != 0 {
            return Err(Error("initialisation de la région impossible"));
        }
        Ok(region)
    }

    /// Map and validate region received from daemon (client side or test).
    pub fn map(object: SharedObject) -> Result<Self, Error> {
        let mut size = 0usize;
        // SAFETY: `object.0` is an owned sharing object; `size` is a valid output pointer.
        let base = unsafe { ffi::lw_shm_map(object.0, &mut size) };
        if base.is_null() {
            return Err(Error("mappage de la région impossible"));
        }
        let obj = object.0;
        std::mem::forget(object); // Object ownership transfers to `Inner`
        let region = Self::wrap(base, size, obj);
        // SAFETY: `base` points to `size` mapped bytes.
        if unsafe { ffi::lw_shm_validate(base, size) } != 0 {
            return Err(Error("région invalide (magie, version ou taille)"));
        }
        Ok(region)
    }

    fn wrap(base: *mut c_void, size: usize, handle: *mut c_void) -> Self {
        Self {
            inner: Arc::new(Inner { base, size, handle }),
            taken: Default::default(),
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

    /// Geometry read from header.
    pub fn geometry(&self) -> Geometry {
        // Read fields immutable after initialization (written before release-published magic).
        let base = self.inner.base.cast::<u32>();
        // SAFETY: header is 4096 bytes; lw_shm_header fixes offsets: sample_rate @12,
        // ring_frames @16, channels @20/@24 (aligned u32 values).
        unsafe {
            Geometry {
                sample_rate: base.add(3).read_volatile(),
                ring_frames: base.add(4).read_volatile(),
                channels_to_net: base.add(5).read_volatile(),
                channels_from_net: base.add(6).read_volatile(),
            }
        }
    }

    /// Channel count for ring `dir`.
    pub fn channels(&self, dir: Dir) -> u32 {
        let g = self.geometry();
        match dir {
            Dir::ToNet => g.channels_to_net,
            Dir::FromNet => g.channels_from_net,
        }
    }

    /// Readable frames in ring `dir`.
    pub fn readable(&self, dir: Dir) -> u32 {
        // SAFETY: valid region; atomic read.
        unsafe { ffi::lw_ring_readable(self.inner.base, dir as c_int) }
    }

    /// Free space (frames) in ring `dir`.
    pub fn writable(&self, dir: Dir) -> u32 {
        // SAFETY: valid region; atomic read.
        unsafe { ffi::lw_ring_writable(self.inner.base, dir as c_int) }
    }

    /// Ring `dir` positions and counters.
    pub fn counters(&self, dir: Dir) -> Counters {
        let mut c = Counters::default();
        // SAFETY: valid region; valid output pointers.
        unsafe {
            ffi::lw_ring_counters(
                self.inner.base,
                dir as c_int,
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
        let flag = self.taken.get(slot).ok_or(Error("extrémité inconnue"))?;
        if flag.swap(true, Ordering::AcqRel) {
            Err(Error("extrémité déjà attribuée"))
        } else {
            Ok(())
        }
    }

    /// Producer for ring `dir` (once per region).
    pub fn producer(&self, dir: Dir) -> Result<Producer, Error> {
        self.take(dir as usize * 2)?;
        Ok(Producer {
            inner: self.inner.clone(),
            dir,
            channels: self.channels(dir),
        })
    }

    /// Consumer for ring `dir` (once per region).
    pub fn consumer(&self, dir: Dir) -> Result<Consumer, Error> {
        self.take(dir as usize * 2 + 1)?;
        Ok(Consumer {
            inner: self.inner.clone(),
            dir,
            channels: self.channels(dir),
        })
    }

    /// Clock writer (daemon, once).
    pub fn clock_writer(&self) -> Result<ClockWriter, Error> {
        self.take(4)?;
        Ok(ClockWriter {
            inner: self.inner.clone(),
        })
    }
}

fn frames_of(len: usize, channels: u32) -> Result<u32, Error> {
    if channels == 0 {
        return Ok(0);
    }
    let ch = channels as usize;
    if len % ch != 0 {
        return Err(Error("longueur non multiple du nombre de canaux"));
    }
    u32::try_from(len / ch).map_err(|_| Error("trop de trames"))
}

/// Ring producer.
pub struct Producer {
    inner: Arc<Inner>,
    dir: Dir,
    channels: u32,
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
        Ok(unsafe {
            ffi::lw_ring_write(
                self.inner.base,
                self.dir as c_int,
                interleaved.as_ptr(),
                frames,
            )
        })
    }
}

/// Ring consumer.
pub struct Consumer {
    inner: Arc<Inner>,
    dir: Dir,
    channels: u32,
}

impl Consumer {
    /// Channels per frame.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Readable frames.
    pub fn readable(&self) -> u32 {
        // SAFETY: valid region; atomic read.
        unsafe { ffi::lw_ring_readable(self.inner.base, self.dir as c_int) }
    }

    /// Fill `out` (interleaved frames); pad missing frames with silence.
    /// Return actual frame count read.
    pub fn read(&mut self, out: &mut [f32]) -> Result<u32, Error> {
        let frames = frames_of(out.len(), self.channels)?;
        // SAFETY: unique consumer for this ring; `out` can hold `frames × channels` samples.
        Ok(unsafe {
            ffi::lw_ring_read(self.inner.base, self.dir as c_int, out.as_mut_ptr(), frames)
        })
    }
}

/// Shared-clock writer.
pub struct ClockWriter {
    inner: Arc<Inner>,
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
        let r = Region::create(48_000, 1024, 8, 2).unwrap();
        assert_eq!(
            r.geometry(),
            Geometry {
                sample_rate: 48_000,
                ring_frames: 1024,
                channels_to_net: 8,
                channels_from_net: 2
            }
        );
        assert_eq!(r.size(), 4096 + 1024 * 10 * 4);
        assert!(
            Region::create(48_000, 1000, 2, 2).is_err(),
            "anneau non puissance de 2"
        );
        assert!(
            Region::create(48_000, 1024, 65, 2).is_err(),
            "trop de canaux"
        );
    }

    #[test]
    fn ends_are_unique() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
        let _p = r.producer(Dir::ToNet).unwrap();
        assert!(r.producer(Dir::ToNet).is_err());
        let _c = r.consumer(Dir::ToNet).unwrap();
        let _w = r.clock_writer().unwrap();
        assert!(r.clock_writer().is_err());
    }

    #[test]
    fn ring_wraps_and_counts() {
        let r = Region::create(48_000, 64, 2, 2).unwrap();
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
            assert_eq!(out, block, "contenu identique après rebouclage");
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
        assert!(p.write(&[1.0]).is_err(), "longueur non multiple des canaux");
    }

    #[test]
    fn host_clock_is_declared() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
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
        Region::map(obj.expect("objet xpc_shmem reçu")).unwrap()
    }

    #[cfg(windows)]
    fn second_mapping(daemon: &Region) -> Region {
        let h = daemon
            .share_with(std::process::id())
            .expect("section dupliquée");
        Region::map(SharedObject::from_handle_value(h).unwrap()).unwrap()
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn spsc_across_two_mappings() {
        let daemon = Region::create(48_000, 1024, 2, 2).unwrap();
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
                assert!(f[0] >= expect, "ordre conservé");
                assert_eq!(f[1], -f[0], "trame intacte (pas de déchirure)");
                expect = f[0] + 1.0;
                got += 1;
            }
        }
        writer.join().unwrap();
        let c = daemon.counters(Dir::ToNet);
        assert_eq!(
            got + c.overruns,
            u64::from(TOTAL),
            "chaque trame est soit lue, soit comptée perdue"
        );
    }

    #[test]
    fn clock_seqlock_is_consistent() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
        assert!(r.clock().is_none(), "aucune horloge avant publication");
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
                assert_eq!(s, h * 2, "triplet cohérent");
                assert_eq!(rate, h as f64 * 0.5);
                reads += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
}
