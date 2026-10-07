//! Livewire clock packet (`docs/protocol/04-clock.md`).
//!
//! RTP + 20-word header extension with profile `0xFA1A`. Partial observed format:
//! sequence (bytes 16–19), type A/B (20–23), master identifier (26–29); remainder unknown.
//! Decode only: transmission is deferred until the complete format is captured.

use crate::bytes::at;
use crate::rtp::Packet;
use crate::Error;

pub const EXTENSION_PROFILE: u16 = 0xFA1A;
pub const EXTENSION_WORDS: usize = 0x14;
/// Expected UDP payload: 12 (RTP) + 4 (extension header) + 80.
pub const EXPECTED_LEN: usize = 96;
/// One packet per 250 µs tick (hypothesis: 12 samples at 48 kHz).
pub const TICK_SAMPLES: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockKind {
    A,
    B,
    Unknown([u8; 4]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPacket {
    /// RTP timestamp: 48 kHz sample counter.
    pub timestamp: u32,
    pub rtp_sequence: u16,
    pub ssrc: u32,
    pub clock_sequence: u32,
    pub kind: ClockKind,
    pub master_id: [u8; 4],
}

impl ClockPacket {
    /// Accept a clock packet: require a 20-word `FA1A` extension.
    pub fn parse(udp_payload: &[u8]) -> Result<Self, Error> {
        let rtp = Packet::parse(udp_payload)?;
        match rtp.extension {
            Some((EXTENSION_PROFILE, data)) if data.len() == EXTENSION_WORDS * 4 => {}
            Some(_) => return Err(Error::Invalid("unexpected clock extension")),
            None => return Err(Error::Invalid("clock packet without extension")),
        }
        let kind = match at::<4>(udp_payload, 20, "Livewire clock")? {
            [0x0A, 0x00, 0xCA, 0xBA] => ClockKind::A,
            [0x0B, 0x00, 0xCA, 0xBA] => ClockKind::B,
            other => ClockKind::Unknown(other),
        };
        Ok(Self {
            timestamp: rtp.timestamp,
            rtp_sequence: rtp.sequence,
            ssrc: rtp.ssrc,
            clock_sequence: u32::from_be_bytes(at(udp_payload, 16, "Livewire clock")?),
            kind,
            master_id: at(udp_payload, 26, "Livewire clock")?,
        })
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn packet(kind: [u8; 4]) -> Vec<u8> {
        let mut v = vec![
            0x90, 96, 0, 1, 0, 0, 0xBB, 0x80, 0xEF, 0xC0, 0xFF, 0x02, 0xFA, 0x1A, 0x00, 0x14,
        ];
        let mut ext = vec![0u8; 80];
        ext[..4].copy_from_slice(&1000u32.to_be_bytes());
        ext[4..8].copy_from_slice(&kind);
        ext[10..14].copy_from_slice(&[0xC0, 0xA8, 0x0A, 0x14]);
        v.extend_from_slice(&ext);
        v
    }

    #[test]
    fn decode() {
        let raw = packet([0x0A, 0x00, 0xCA, 0xBA]);
        assert_eq!(raw.len(), EXPECTED_LEN);
        let c = ClockPacket::parse(&raw).unwrap();
        assert_eq!(
            (c.timestamp, c.clock_sequence, c.kind, c.master_id),
            (48_000, 1000, ClockKind::A, [0xC0, 0xA8, 0x0A, 0x14])
        );
        assert_eq!(
            ClockPacket::parse(&packet([0x0B, 0, 0xCA, 0xBA]))
                .unwrap()
                .kind,
            ClockKind::B
        );
        assert!(ClockPacket::parse(&raw[..40]).is_err());
        let mut no_ext = raw.clone();
        no_ext[0] = 0x80;
        assert!(ClockPacket::parse(&no_ext).is_err());
    }
}
