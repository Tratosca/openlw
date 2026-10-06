//! Stream types and packet sizes (`docs/protocol/02-rtp-audio.md`).

use crate::channel::GroupKind;

/// Livewire sample rate.
pub const SAMPLE_RATE: u32 = 48_000;

/// Maximum Livewire audio-packet payload.
pub const MAX_PAYLOAD: usize = 1440;

/// Livewire audio-stream type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamFormat {
    /// 240 samples (5 ms), stereo.
    Standard,
    /// 48 samples (1 ms), stereo, PTP media clock.
    Aes67,
    /// 12 samples (0.25 ms), stereo.
    Livestream,
    /// 60 samples (1.25 ms), eight channels.
    Surround,
}

impl StreamFormat {
    pub const fn samples_per_packet(self) -> u32 {
        match self {
            StreamFormat::Standard => 240,
            StreamFormat::Aes67 => 48,
            StreamFormat::Livestream => 12,
            StreamFormat::Surround => 60,
        }
    }

    pub const fn channels(self) -> u16 {
        match self {
            StreamFormat::Surround => 8,
            _ => 2,
        }
    }

    pub const fn group_kind(self) -> GroupKind {
        match self {
            StreamFormat::Surround => GroupKind::Surround,
            _ => GroupKind::Stereo,
        }
    }

    /// Packet interval in microseconds.
    pub const fn packet_interval_us(self) -> u32 {
        self.samples_per_packet() * 1_000_000 / SAMPLE_RATE
    }

    /// L24 payload.
    pub const fn payload_bytes_l24(self) -> usize {
        self.samples_per_packet() as usize * self.channels() as usize * 3
    }

    /// Format matching an observed packet size and channel count.
    pub fn from_samples(samples: u32, channels: u16) -> Option<Self> {
        [
            StreamFormat::Standard,
            StreamFormat::Aes67,
            StreamFormat::Livestream,
            StreamFormat::Surround,
        ]
        .into_iter()
        .find(|f| f.samples_per_packet() == samples && f.channels() == channels)
    }
}

/// `a=ptime` value:
/// Multiple of 48 samples → integer milliseconds; 60 or 12 → two decimals; otherwise “1”.
pub fn ptime_text(samples: u32) -> String {
    if samples % 48 == 0 {
        (samples * 1000 / SAMPLE_RATE).to_string()
    } else if samples == 60 || samples == 12 {
        format!(
            "{:.2}",
            f64::from(samples) * 1000.0 / f64::from(SAMPLE_RATE)
        )
    } else {
        "1".to_string()
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(StreamFormat::Standard.payload_bytes_l24(), 1440);
        assert_eq!(StreamFormat::Surround.payload_bytes_l24(), 1440);
        assert_eq!(StreamFormat::Aes67.payload_bytes_l24(), 288);
        assert_eq!(StreamFormat::Livestream.packet_interval_us(), 250);
        assert_eq!(StreamFormat::Surround.packet_interval_us(), 1250);
        assert_eq!(
            StreamFormat::from_samples(60, 8),
            Some(StreamFormat::Surround)
        );
    }

    #[test]
    fn ptime() {
        assert_eq!(ptime_text(48), "1");
        assert_eq!(ptime_text(240), "5");
        assert_eq!(ptime_text(60), "1.25");
        assert_eq!(ptime_text(12), "0.25");
        assert_eq!(ptime_text(100), "1");
    }
}
