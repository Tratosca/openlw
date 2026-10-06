//! RTP (RFC 3550) et charge L24/L16 (`docs/protocol/02-rtp-audio.md`).

use std::net::Ipv4Addr;

use crate::bytes::Reader;
use crate::format::StreamFormat;
use crate::Error;

pub const HEADER_LEN: usize = 12;

/// En-tête RTP décodé ; `payload` désigne la charge, hors CSRC, extension et bourrage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<'a> {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub csrc: Vec<u32>,
    /// (profil, données) de l'extension d'en-tête, si présente.
    pub extension: Option<(u16, &'a [u8])>,
    pub payload: &'a [u8],
}

impl<'a> Packet<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf, "paquet RTP");
        let b0 = r.u8()?;
        if b0 >> 6 != 2 {
            return Err(Error::Invalid("version RTP différente de 2"));
        }
        let b1 = r.u8()?;
        let sequence = r.u16()?;
        let timestamp = r.u32()?;
        let ssrc = r.u32()?;
        let mut csrc = Vec::new();
        for _ in 0..(b0 & 0x0F) {
            csrc.push(r.u32()?);
        }
        let extension = if b0 & 0x10 != 0 {
            let profile = r.u16()?;
            let words = usize::from(r.u16()?);
            Some((profile, r.take(words * 4)?))
        } else {
            None
        };
        let mut payload = r.rest();
        if b0 & 0x20 != 0 {
            let pad = usize::from(
                *payload
                    .last()
                    .ok_or(Error::Invalid("bourrage sans octet"))?,
            );
            if pad == 0 || pad > payload.len() {
                return Err(Error::Invalid("bourrage RTP"));
            }
            payload = payload
                .get(..payload.len() - pad)
                .ok_or(Error::Invalid("bourrage RTP"))?;
        }
        Ok(Self {
            marker: b1 & 0x80 != 0,
            payload_type: b1 & 0x7F,
            sequence,
            timestamp,
            ssrc,
            csrc,
            extension,
            payload,
        })
    }
}

/// Écrit un en-tête RTP de 12 octets (V=2, sans CSRC ni extension).
pub fn write_header(
    out: &mut Vec<u8>,
    payload_type: u8,
    marker: bool,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
) {
    out.push(0x80);
    out.push((payload_type & 0x7F) | if marker { 0x80 } else { 0 });
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
}

/// SSRC choisi par OpenLW : les 4 octets de l'adresse de destination (unique par flux, stable).
pub fn ssrc_from_group(group: Ipv4Addr) -> u32 {
    u32::from(group)
}

/// Échantillon 24 bits signé (dans un i32) → 3 octets big-endian. Les valeurs hors plage sont saturées.
pub fn put_l24(out: &mut Vec<u8>, sample: i32) {
    let s = sample.clamp(-(1 << 23), (1 << 23) - 1);
    let [_, b1, b2, b3] = s.to_be_bytes();
    out.extend_from_slice(&[b1, b2, b3]);
}

/// 3 octets big-endian → échantillon 24 bits signé.
pub fn get_l24(b: [u8; 3]) -> i32 {
    i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8
}

/// Décode une charge L24 entrelacée vers `out` (échantillons 24 bits dans des i32).
/// Retourne le nombre d'échantillons écrits ; un octet de queue incomplet est ignoré.
pub fn decode_l24(payload: &[u8], out: &mut Vec<i32>) -> usize {
    let before = out.len();
    out.extend(
        payload
            .chunks_exact(3)
            .filter_map(|c| c.try_into().ok())
            .map(get_l24),
    );
    out.len() - before
}

/// Paquetiseur : découpe un flux entrelacé en paquets RTP L24 au format donné.
#[derive(Debug, Clone)]
pub struct Packetizer {
    format: StreamFormat,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
}

impl Packetizer {
    pub fn new(
        format: StreamFormat,
        payload_type: u8,
        ssrc: u32,
        first_sequence: u16,
        first_timestamp: u32,
    ) -> Self {
        Self {
            format,
            payload_type,
            ssrc,
            sequence: first_sequence,
            timestamp: first_timestamp,
        }
    }

    pub fn format(&self) -> StreamFormat {
        self.format
    }

    /// Nombre d'échantillons entrelacés attendus par paquet (trames × canaux).
    pub fn samples_needed(&self) -> usize {
        self.format.samples_per_packet() as usize * usize::from(self.format.channels())
    }

    /// Repositionne le timestamp (resynchronisation sur une horloge média, AES67).
    pub fn set_timestamp(&mut self, timestamp: u32) {
        self.timestamp = timestamp;
    }

    /// Construit le paquet suivant à partir de `samples` (exactement `samples_needed()` valeurs).
    pub fn packet(&mut self, samples: &[i32], out: &mut Vec<u8>) -> Result<(), Error> {
        if samples.len() != self.samples_needed() {
            return Err(Error::Invalid("nombre d'échantillons du paquet"));
        }
        out.clear();
        out.reserve(HEADER_LEN + samples.len() * 3);
        write_header(
            out,
            self.payload_type,
            false,
            self.sequence,
            self.timestamp,
            self.ssrc,
        );
        for &s in samples {
            put_l24(out, s);
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self
            .timestamp
            .wrapping_add(self.format.samples_per_packet());
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn l24_roundtrip_and_saturation() {
        let mut v = Vec::new();
        for s in [0, 1, -1, 8_388_607, -8_388_608, 123_456, -654_321] {
            v.clear();
            put_l24(&mut v, s);
            assert_eq!(get_l24([v[0], v[1], v[2]]), s);
        }
        v.clear();
        put_l24(&mut v, i32::MAX);
        assert_eq!(get_l24([v[0], v[1], v[2]]), 8_388_607);
    }

    #[test]
    fn packetizer_standard() {
        let mut p = Packetizer::new(StreamFormat::Standard, 96, 0xEFC0_0065, 10, 1000);
        let samples = vec![0i32; p.samples_needed()];
        let mut out = Vec::new();
        p.packet(&samples, &mut out).unwrap();
        assert_eq!(out.len(), 12 + 1440);
        let pkt = Packet::parse(&out).unwrap();
        assert_eq!(
            (pkt.payload_type, pkt.sequence, pkt.timestamp, pkt.ssrc),
            (96, 10, 1000, 0xEFC0_0065)
        );
        p.packet(&samples, &mut out).unwrap();
        let pkt = Packet::parse(&out).unwrap();
        assert_eq!((pkt.sequence, pkt.timestamp), (11, 1240));
        assert!(p.packet(&samples[1..], &mut out).is_err());
    }

    #[test]
    fn parse_extension_and_padding() {
        let mut buf = vec![
            0xB0, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0xFA, 0x1A, 0, 1, 1, 2, 3, 4, 9, 9, 0, 3,
        ];
        let pkt = Packet::parse(&buf).unwrap();
        assert_eq!(pkt.extension, Some((0xFA1A, &[1u8, 2, 3, 4][..])));
        assert_eq!(pkt.payload, &[9]);
        buf[23] = 9; // bourrage plus long que la charge
        assert!(Packet::parse(&buf).is_err());
    }
}
