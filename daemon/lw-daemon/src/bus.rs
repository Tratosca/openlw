//! Audio bus between daemon threads: lock-free SPSC queue of interleaved float32 samples,
//! in safe Rust (samples stored in `AtomicU32`), with a jitter-buffer reader.
//!
//! Usage: received RTP → bus → device inputs; device outputs → bus → transmitted stream.
//! The endpoints follow different clocks (remote transmitter versus host clock):
//! the [`JitterReader`] primes, slips (discards excess) above the high threshold, and
//! restarts priming after an underrun. This compensates through slips rather than
//! resampling: occasional audible jumps until clock synchronization is implemented (ADR 0003).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared {
    samples: Box<[AtomicU32]>,
    channels: usize,
    capacity: usize, // In frames
    write: AtomicUsize,
    read: AtomicUsize,
    overruns: AtomicU64,
    underruns: AtomicU64,
    slips: AtomicU64,
    /// Jitter buffer primed: stored on the bus to survive reader replacement.
    primed: AtomicBool,
}

/// Bus counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct BusCounters {
    pub written: u64,
    pub read: u64,
    pub overruns: u64,
    pub underruns: u64,
    /// Frames discarded by slips (transmitter drift).
    pub slips: u64,
}

/// Create a bus with `capacity` frames and `channels` channels.
pub fn bus(channels: usize, capacity: usize) -> (BusWriter, BusReader) {
    let p = port(channels, capacity);
    (p.writer(), p.reader())
}

/// Bus attachment point: provides new endpoints for the same bus.
///
/// Used for live patching: the device thread replaces the entire routing table,
/// dropping the old endpoint before using the new one. Invariant: at any
/// time, at most one writing endpoint and one reading endpoint are used.
#[derive(Clone)]
pub struct Port {
    s: Arc<Shared>,
}

impl Port {
    pub fn writer(&self) -> BusWriter {
        BusWriter { s: self.s.clone() }
    }

    pub fn reader(&self) -> BusReader {
        BusReader { s: self.s.clone() }
    }

    pub fn channels(&self) -> usize {
        self.s.channels
    }
}

/// Create a bus and return its attachment point.
pub fn port(channels: usize, capacity: usize) -> Port {
    let channels = channels.max(1);
    let capacity = capacity.max(1);
    let shared = Arc::new(Shared {
        samples: (0..channels * capacity)
            .map(|_| AtomicU32::new(0))
            .collect(),
        channels,
        capacity,
        write: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
        overruns: AtomicU64::new(0),
        underruns: AtomicU64::new(0),
        slips: AtomicU64::new(0),
        primed: AtomicBool::new(false),
    });
    Port { s: shared }
}

impl Shared {
    fn available(&self) -> usize {
        self.write
            .load(Ordering::Acquire)
            .wrapping_sub(self.read.load(Ordering::Acquire))
    }

    fn counters(&self) -> BusCounters {
        BusCounters {
            written: self.write.load(Ordering::Relaxed) as u64,
            read: self.read.load(Ordering::Relaxed) as u64,
            overruns: self.overruns.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            slips: self.slips.load(Ordering::Relaxed),
        }
    }
}

/// Unique writing endpoint.
pub struct BusWriter {
    s: Arc<Shared>,
}

impl BusWriter {
    pub fn channels(&self) -> usize {
        self.s.channels
    }

    /// Write interleaved frames; return the accepted count (the remainder counts as overrun).
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let ch = self.s.channels;
        let frames = interleaved.len() / ch;
        let w = self.s.write.load(Ordering::Relaxed);
        let free = self.s.capacity - self.s.available().min(self.s.capacity);
        let n = frames.min(free);
        for (i, frame) in interleaved.chunks_exact(ch).take(n).enumerate() {
            let base = (w.wrapping_add(i) % self.s.capacity) * ch;
            for (c, v) in frame.iter().enumerate() {
                if let Some(slot) = self.s.samples.get(base + c) {
                    slot.store(v.to_bits(), Ordering::Relaxed);
                }
            }
        }
        self.s.write.store(w.wrapping_add(n), Ordering::Release);
        if n < frames {
            self.s
                .overruns
                .fetch_add((frames - n) as u64, Ordering::Relaxed);
        }
        n
    }

    pub fn counters(&self) -> BusCounters {
        self.s.counters()
    }
}

/// Unique reading endpoint.
pub struct BusReader {
    s: Arc<Shared>,
}

impl BusReader {
    pub fn channels(&self) -> usize {
        self.s.channels
    }

    pub fn available(&self) -> usize {
        self.s.available()
    }

    /// Read up to `out.len() / channels` frames; pad with silence and count missing frames.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        let ch = self.s.channels;
        let frames = out.len() / ch;
        let r = self.s.read.load(Ordering::Relaxed);
        let n = frames.min(self.s.available());
        for (i, frame) in out.chunks_exact_mut(ch).enumerate() {
            if i < n {
                let base = (r.wrapping_add(i) % self.s.capacity) * ch;
                for (c, v) in frame.iter_mut().enumerate() {
                    *v = self
                        .s
                        .samples
                        .get(base + c)
                        .map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)));
                }
            } else {
                frame.iter_mut().for_each(|v| *v = 0.0);
            }
        }
        self.s.read.store(r.wrapping_add(n), Ordering::Release);
        if n < frames {
            self.s
                .underruns
                .fetch_add((frames - n) as u64, Ordering::Relaxed);
        }
        n
    }

    /// Discard `frames` frames (slip).
    fn skip(&mut self, frames: usize) {
        let n = frames.min(self.s.available());
        let r = self.s.read.load(Ordering::Relaxed);
        self.s.read.store(r.wrapping_add(n), Ordering::Release);
        self.s.slips.fetch_add(n as u64, Ordering::Relaxed);
    }

    pub fn counters(&self) -> BusCounters {
        self.s.counters()
    }
}

/// Jitter-buffer reader: prime to `target` frames, slip above `high`.
pub struct JitterReader {
    reader: BusReader,
    target: usize,
    high: usize,
}

impl JitterReader {
    pub fn new(reader: BusReader, target: usize, high: usize) -> Self {
        Self {
            reader,
            target,
            high: high.max(target + 1),
        }
    }

    pub fn channels(&self) -> usize {
        self.reader.channels()
    }

    pub fn counters(&self) -> BusCounters {
        self.reader.counters()
    }

    pub fn primed(&self) -> bool {
        self.reader.s.primed.load(Ordering::Relaxed)
    }

    fn set_primed(&self, on: bool) {
        self.reader.s.primed.store(on, Ordering::Relaxed);
    }

    /// Fill `out`: silence until primed; otherwise data, with
    /// slips if excess exceeds `high` and repriming after an underrun.
    pub fn pull(&mut self, out: &mut [f32]) {
        let avail = self.reader.available();
        if !self.primed() {
            if avail >= self.target {
                self.set_primed(true);
            } else {
                out.iter_mut().for_each(|v| *v = 0.0);
                return;
            }
        }
        if avail > self.high {
            self.reader.skip(avail - self.target);
        }
        let want = out.len() / self.reader.channels();
        if self.reader.pull(out) < want {
            self.set_primed(false);
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn spsc_order_and_counters() {
        let (mut w, mut r) = bus(2, 8);
        assert_eq!(w.push(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]), 3);
        let mut out = [0f32; 4];
        assert_eq!(r.pull(&mut out), 2);
        assert_eq!(out, [1.0, -1.0, 2.0, -2.0]);
        assert_eq!(w.push(&[9.0; 20]), 7, "capacité 8, 1 trame encore présente");
        assert_eq!(w.counters().overruns, 3);
        let mut big = [5f32; 20];
        assert_eq!(r.pull(&mut big), 8);
        assert_eq!(big[16..], [0.0; 4], "manque complété par du silence");
        assert_eq!(r.counters().underruns, 2);
    }

    #[test]
    fn threads_preserve_sequence() {
        let (mut w, mut r) = bus(2, 512);
        let t = std::thread::spawn(move || {
            let mut n = 0u32;
            while n < 100_000 {
                let block: Vec<f32> = (n..n + 30).flat_map(|i| [i as f32, -(i as f32)]).collect();
                let k = w.push(&block) as u32;
                n += k;
                if k == 0 {
                    std::thread::yield_now();
                }
            }
        });
        let mut expect = 0f32;
        let mut buf = [0f32; 2 * 17];
        while expect < 100_000.0 {
            let n = r.available().min(17);
            if n == 0 {
                std::thread::yield_now();
                continue;
            }
            r.pull(&mut buf[..2 * n]);
            for f in buf[..2 * n].chunks_exact(2) {
                assert_eq!((f[0], f[1]), (expect, -expect));
                expect += 1.0;
            }
        }
        t.join().unwrap();
    }

    #[test]
    fn replaced_reader_keeps_position_and_priming() {
        let p = port(1, 1024);
        let mut w = p.writer();
        let mut j = JitterReader::new(p.reader(), 10, 100);
        w.push(&(0..20).map(|i| i as f32).collect::<Vec<_>>());
        let mut out = [0f32; 5];
        j.pull(&mut out);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0]);
        // Live patch: new reader on the same bus, without repriming or jumping.
        let mut j2 = JitterReader::new(p.reader(), 10, 100);
        assert!(j2.primed());
        j2.pull(&mut out);
        assert_eq!(out, [5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn jitter_reader_primes_slips_and_reprimes() {
        let (mut w, r) = bus(1, 4096);
        let mut j = JitterReader::new(r, 100, 400);
        let mut out = [7f32; 48];
        w.push(&[1.0; 50]);
        j.pull(&mut out);
        assert!(
            !j.primed() && out.iter().all(|&v| v == 0.0),
            "silence avant amorçage"
        );
        w.push(&[1.0; 60]);
        j.pull(&mut out);
        assert!(j.primed() && out.iter().all(|&v| v == 1.0));
        // Transmitter too fast: 1000 queued frames → slip to target.
        w.push(&[2.0; 1000]);
        j.pull(&mut out);
        assert!(j.counters().slips > 0);
        assert!(j.reader.available() <= 100);
        // Underrun: reprime.
        let mut big = [0f32; 480];
        j.pull(&mut big);
        assert!(!j.primed());
    }
}
