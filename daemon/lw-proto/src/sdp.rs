//! SDP AES67 (`docs/protocol/02-rtp-audio.md`).
//!
//! Génération (RFC 4566, RFC 7273, AES67) et analyse tolérante des champs dont un récepteur a besoin.

use std::fmt::Write as _;
use std::net::Ipv4Addr;

use crate::format::{ptime_text, SAMPLE_RATE};
use crate::Error;

/// Sens du flux (`a=sendonly` / `recvonly` / `inactive`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    SendOnly,
    RecvOnly,
    Inactive,
}

/// Référence d'horloge PTP (`a=ts-refclk` + `a=mediaclk:direct=<offset>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtpRef {
    pub grandmaster: [u8; 8],
    pub domain: u8,
    pub media_clock_offset: u32,
}

/// Description d'un flux audio RTP linéaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: u64,
    pub session_version: u64,
    pub origin: Ipv4Addr,
    pub name: String,
    pub group: Ipv4Addr,
    pub ttl: Option<u8>,
    pub port: u16,
    pub payload_type: u8,
    pub bits: u8,
    pub rate: u32,
    pub channels: u16,
    pub direction: Direction,
    /// Échantillons par paquet (pour `a=ptime`).
    pub samples_per_packet: u32,
    pub max_samples_per_packet: Option<u32>,
    pub ptp: Option<PtpRef>,
}

impl Session {
    /// Texte SDP, fins de ligne CRLF.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let ttl = self.ttl.map(|t| format!("/{t}")).unwrap_or_default();
        let dir = match self.direction {
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::Inactive => "inactive",
        };
        // write! sur une String ne peut pas échouer.
        let _ = write!(
            s,
            "v=0\r\no=- {} {} IN IP4 {}\r\ns={}\r\nc=IN IP4 {}{}\r\nt=0 0\r\nm=audio {} RTP/AVP {}\r\n\
             a=rtpmap:{} L{}/{}/{}\r\na={}\r\na=ptime:{}\r\n",
            self.session_id,
            self.session_version,
            self.origin,
            self.name,
            self.group,
            ttl,
            self.port,
            self.payload_type,
            self.payload_type,
            self.bits,
            self.rate,
            self.channels,
            dir,
            ptime_text(self.samples_per_packet),
        );
        if let Some(max) = self.max_samples_per_packet {
            let _ = write!(s, "a=maxptime:{}\r\n", ptime_text(max));
        }
        if let Some(p) = self.ptp {
            let g = p.grandmaster;
            let _ = write!(
                s,
                "a=ts-refclk:ptp=IEEE1588-2008:{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}:{}\r\na=mediaclk:direct={}\r\n",
                g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7], p.domain, p.media_clock_offset
            );
        }
        s
    }

    /// Analyse un SDP ; seuls `c=`, `m=` et `a=rtpmap` sont obligatoires.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut sess = Session {
            session_id: 0,
            session_version: 0,
            origin: Ipv4Addr::UNSPECIFIED,
            name: String::new(),
            group: Ipv4Addr::UNSPECIFIED,
            ttl: None,
            port: 0,
            payload_type: 0,
            bits: 0,
            rate: 0,
            channels: 0,
            direction: Direction::SendOnly,
            samples_per_packet: 0,
            max_samples_per_packet: None,
            ptp: None,
        };
        let (mut have_c, mut have_m, mut have_map) = (false, false, false);
        let mut refclk: Option<([u8; 8], u8)> = None;
        let mut mediaclk: Option<u32> = None;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(v) = line.strip_prefix("o=") {
                let f: Vec<&str> = v.split_whitespace().collect();
                sess.session_id = f.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
                sess.session_version = f.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);
                sess.origin = f
                    .get(5)
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(Ipv4Addr::UNSPECIFIED);
            } else if let Some(v) = line.strip_prefix("s=") {
                sess.name = v.to_string();
            } else if let Some(v) = line.strip_prefix("c=IN IP4 ") {
                let mut parts = v.split('/');
                sess.group = parts
                    .next()
                    .unwrap_or("")
                    .parse()
                    .map_err(|_| Error::Invalid("adresse c="))?;
                sess.ttl = parts.next().and_then(|t| t.parse().ok());
                have_c = true;
            } else if let Some(v) = line.strip_prefix("m=audio ") {
                let f: Vec<&str> = v.split_whitespace().collect();
                sess.port = f
                    .first()
                    .and_then(|x| x.parse().ok())
                    .ok_or(Error::Invalid("port m="))?;
                sess.payload_type = f
                    .get(2)
                    .and_then(|x| x.parse().ok())
                    .ok_or(Error::Invalid("PT m="))?;
                have_m = true;
            } else if let Some(v) = line.strip_prefix("a=rtpmap:") {
                let (pt, enc) = v.split_once(' ').ok_or(Error::Invalid("rtpmap"))?;
                if pt.parse::<u8>().ok() != Some(sess.payload_type) && have_m {
                    continue;
                }
                let mut f = enc.split('/');
                let codec = f.next().unwrap_or("");
                sess.bits = codec
                    .strip_prefix('L')
                    .and_then(|b| b.parse().ok())
                    .ok_or(Error::Invalid("codec non linéaire"))?;
                sess.rate = f
                    .next()
                    .and_then(|x| x.parse().ok())
                    .ok_or(Error::Invalid("fréquence rtpmap"))?;
                sess.channels = f.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                have_map = true;
            } else if let Some(v) = line.strip_prefix("a=ptime:") {
                sess.samples_per_packet = ms_to_samples(v).ok_or(Error::Invalid("ptime"))?;
            } else if let Some(v) = line.strip_prefix("a=maxptime:") {
                sess.max_samples_per_packet = ms_to_samples(v);
            } else if line == "a=sendonly" {
                sess.direction = Direction::SendOnly;
            } else if line == "a=recvonly" {
                sess.direction = Direction::RecvOnly;
            } else if line == "a=inactive" {
                sess.direction = Direction::Inactive;
            } else if let Some(v) = line.strip_prefix("a=ts-refclk:ptp=IEEE1588-2008:") {
                refclk = parse_refclk(v);
            } else if let Some(v) = line.strip_prefix("a=mediaclk:direct=") {
                mediaclk = v.split_whitespace().next().and_then(|x| x.parse().ok());
            } else if let Some(v) = line.strip_prefix("a=sync-time:") {
                // Variante Ravenna.
                mediaclk = mediaclk.or_else(|| v.trim().parse().ok());
            }
        }
        if !(have_c && have_m && have_map) {
            return Err(Error::Invalid(
                "SDP incomplet : c=, m=audio et a=rtpmap requis",
            ));
        }
        if sess.samples_per_packet == 0 {
            sess.samples_per_packet = SAMPLE_RATE / 1000;
        }
        sess.ptp = refclk.map(|(grandmaster, domain)| PtpRef {
            grandmaster,
            domain,
            media_clock_offset: mediaclk.unwrap_or(0),
        });
        Ok(sess)
    }
}

fn ms_to_samples(v: &str) -> Option<u32> {
    let ms: f64 = v.trim().parse().ok()?;
    let samples = (ms * f64::from(SAMPLE_RATE) / 1000.0).round();
    (1.0..=48_000.0)
        .contains(&samples)
        .then_some(samples as u32)
}

fn parse_refclk(v: &str) -> Option<([u8; 8], u8)> {
    let (id, domain) = v.trim().rsplit_once(':')?;
    let bytes: Vec<u8> = id
        .split('-')
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect();
    let gm: [u8; 8] = bytes.try_into().ok()?;
    Some((gm, domain.parse().ok()?))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            session_id: 0,
            session_version: 0,
            origin: Ipv4Addr::new(192, 168, 10, 20),
            name: "MAC TEST 1".into(),
            group: Ipv4Addr::new(239, 192, 0, 101),
            ttl: None,
            port: 5004,
            payload_type: 96,
            bits: 24,
            rate: 48_000,
            channels: 2,
            direction: Direction::SendOnly,
            samples_per_packet: 48,
            max_samples_per_packet: None,
            ptp: Some(PtpRef {
                grandmaster: [0x00, 0x1D, 0xC1, 0xFF, 0xFE, 0x12, 0x34, 0x56],
                domain: 0,
                media_clock_offset: 0,
            }),
        }
    }

    #[test]
    fn template_and_roundtrip() {
        let text = sample().to_text();
        assert!(text.starts_with("v=0\r\no=- 0 0 IN IP4 192.168.10.20\r\ns=MAC TEST 1\r\nc=IN IP4 239.192.0.101\r\nt=0 0\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\na=sendonly\r\na=ptime:1\r\n"));
        assert!(text.ends_with(
            "a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0\r\na=mediaclk:direct=0\r\n"
        ));
        assert_eq!(Session::parse(&text).unwrap(), sample());
    }

    #[test]
    fn rejects_incomplete() {
        assert!(Session::parse("v=0\r\nc=IN IP4 239.192.0.1\r\n").is_err());
        assert!(
            Session::parse("c=IN IP4 x\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2")
                .is_err()
        );
    }
}
