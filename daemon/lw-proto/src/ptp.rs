//! PTPv2 (IEEE 1588-2008), messages relevant to the AES67 media profile (`docs/protocol/04-clock.md`).

use std::net::Ipv4Addr;

use crate::bytes::{at, Reader};
use crate::format::SAMPLE_RATE;
use crate::Error;

pub const PRIMARY_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);
pub const EVENT_PORT: u16 = 319;
pub const GENERAL_PORT: u16 = 320;
pub const HEADER_LEN: usize = 34;
/// TAI − UTC offset in effect since 2017.
pub const TAI_UTC_OFFSET: i16 = 37;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Sync,
    DelayReq,
    PdelayReq,
    PdelayResp,
    FollowUp,
    DelayResp,
    PdelayRespFollowUp,
    Announce,
    Signaling,
    Management,
    Other(u8),
}

impl MessageType {
    pub const fn code(self) -> u8 {
        match self {
            MessageType::Sync => 0x0,
            MessageType::DelayReq => 0x1,
            MessageType::PdelayReq => 0x2,
            MessageType::PdelayResp => 0x3,
            MessageType::FollowUp => 0x8,
            MessageType::DelayResp => 0x9,
            MessageType::PdelayRespFollowUp => 0xA,
            MessageType::Announce => 0xB,
            MessageType::Signaling => 0xC,
            MessageType::Management => 0xD,
            MessageType::Other(c) => c & 0x0F,
        }
    }

    pub const fn from_code(c: u8) -> Self {
        match c & 0x0F {
            0x0 => MessageType::Sync,
            0x1 => MessageType::DelayReq,
            0x2 => MessageType::PdelayReq,
            0x3 => MessageType::PdelayResp,
            0x8 => MessageType::FollowUp,
            0x9 => MessageType::DelayResp,
            0xA => MessageType::PdelayRespFollowUp,
            0xB => MessageType::Announce,
            0xC => MessageType::Signaling,
            0xD => MessageType::Management,
            other => MessageType::Other(other),
        }
    }

    const fn control(self) -> u8 {
        match self {
            MessageType::Sync => 0,
            MessageType::DelayReq => 1,
            MessageType::FollowUp => 2,
            MessageType::DelayResp => 3,
            _ => 5,
        }
    }

    const fn length(self) -> u16 {
        match self {
            MessageType::DelayResp | MessageType::PdelayResp | MessageType::PdelayRespFollowUp => {
                54
            }
            MessageType::Announce => 64,
            _ => 44,
        }
    }
}

/// Port identity: clock identity (EUI-64) and port number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortIdentity {
    pub clock: [u8; 8],
    pub port: u16,
}

impl PortIdentity {
    /// EUI-64 derived from a MAC address (insert FF FE).
    pub fn from_mac(mac: [u8; 6], port: u16) -> Self {
        Self {
            clock: [mac[0], mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5]],
            port,
        }
    }
}

/// PTP timestamp in nanoseconds since the PTP epoch (TAI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u128);

impl Timestamp {
    pub fn from_parts(seconds: u64, nanoseconds: u32) -> Self {
        Self(u128::from(seconds) * 1_000_000_000 + u128::from(nanoseconds))
    }

    pub fn seconds(self) -> u64 {
        (self.0 / 1_000_000_000) as u64
    }

    pub fn nanoseconds(self) -> u32 {
        (self.0 % 1_000_000_000) as u32
    }

    fn write(self, out: &mut Vec<u8>) {
        let s = self.seconds() & 0xFFFF_FFFF_FFFF;
        out.extend_from_slice(&s.to_be_bytes()[2..]);
        out.extend_from_slice(&self.nanoseconds().to_be_bytes());
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let raw: [u8; 6] = r.array()?;
        let s = u64::from_be_bytes([0, 0, raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]]);
        let ns = r.u32()?;
        if ns >= 1_000_000_000 {
            return Err(Error::Invalid("nanosecondes PTP >= 10^9"));
        }
        Ok(Self::from_parts(s, ns))
    }

    /// AES67 media clock (`mediaclk:direct=offset`): 48 kHz samples, modulo 2^32.
    /// AES67 media clock (RFC 7273, `mediaclk:direct=0`): `s × 48000 + ns × 48000 / 10^9`.
    pub fn media_clock(self, offset: u32) -> u32 {
        let samples = self.0 * u128::from(SAMPLE_RATE) / 1_000_000_000;
        (samples as u32).wrapping_add(offset)
    }
}

/// Advertised clock quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockQuality {
    pub class: u8,
    pub accuracy: u8,
    pub variance: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announce {
    pub utc_offset: i16,
    pub priority1: u8,
    pub quality: ClockQuality,
    pub priority2: u8,
    pub grandmaster: [u8; 8],
    pub steps_removed: u16,
    pub time_source: u8,
}

/// Message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// Sync, Delay_Req, Follow_Up: origin timestamp.
    Timestamp(Timestamp),
    DelayResp {
        receive: Timestamp,
        requesting: PortIdentity,
    },
    Announce {
        origin: Timestamp,
        announce: Announce,
    },
    /// Undecoded type.
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub msg_type: MessageType,
    pub version: u8,
    pub domain: u8,
    pub flags: u16,
    pub correction: i64,
    pub source: PortIdentity,
    pub sequence: u16,
    pub log_interval: i8,
    pub body: Body,
}

impl Message {
    pub const FLAG_TWO_STEP: u16 = 0x0200;
    pub const FLAG_UTC_OFFSET_VALID: u16 = 0x0004;
    pub const FLAG_PTP_TIMESCALE: u16 = 0x0008;

    pub fn two_step(&self) -> bool {
        self.flags & Self::FLAG_TWO_STEP != 0
    }

    pub fn new(
        msg_type: MessageType,
        domain: u8,
        source: PortIdentity,
        sequence: u16,
        log_interval: i8,
        body: Body,
    ) -> Self {
        Self {
            msg_type,
            version: 2,
            domain,
            flags: 0,
            correction: 0,
            source,
            sequence,
            log_interval,
            body,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.push(self.msg_type.code());
        out.push(self.version & 0x0F);
        out.extend_from_slice(&self.msg_type.length().to_be_bytes());
        out.push(self.domain);
        out.push(0);
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.correction.to_be_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&self.source.clock);
        out.extend_from_slice(&self.source.port.to_be_bytes());
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.push(self.msg_type.control());
        out.push(self.log_interval as u8);
        match self.body {
            Body::Timestamp(t) => t.write(&mut out),
            Body::DelayResp {
                receive,
                requesting,
            } => {
                receive.write(&mut out);
                out.extend_from_slice(&requesting.clock);
                out.extend_from_slice(&requesting.port.to_be_bytes());
            }
            Body::Announce {
                origin,
                announce: a,
            } => {
                origin.write(&mut out);
                out.extend_from_slice(&a.utc_offset.to_be_bytes());
                out.push(0);
                out.push(a.priority1);
                out.extend_from_slice(&[a.quality.class, a.quality.accuracy]);
                out.extend_from_slice(&a.quality.variance.to_be_bytes());
                out.push(a.priority2);
                out.extend_from_slice(&a.grandmaster);
                out.extend_from_slice(&a.steps_removed.to_be_bytes());
                out.push(a.time_source);
            }
            Body::Raw => {}
        }
        out.resize(usize::from(self.msg_type.length()), 0);
        out
    }

    pub fn parse(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf, "message PTP");
        let msg_type = MessageType::from_code(r.u8()?);
        let version = r.u8()? & 0x0F;
        if version != 2 {
            return Err(Error::Invalid("version PTP différente de 2"));
        }
        let length = usize::from(r.u16()?);
        if length < HEADER_LEN || length > buf.len() {
            return Err(Error::Invalid("longueur de message PTP"));
        }
        let domain = r.u8()?;
        r.u8()?;
        let flags = r.u16()?;
        let correction = i64::from_be_bytes(r.array()?);
        r.take(4)?;
        let source = PortIdentity {
            clock: r.array()?,
            port: r.u16()?,
        };
        let sequence = r.u16()?;
        r.u8()?;
        let log_interval = r.u8()? as i8;
        let body = match msg_type {
            MessageType::Sync | MessageType::DelayReq | MessageType::FollowUp => {
                Body::Timestamp(Timestamp::read(&mut r)?)
            }
            MessageType::DelayResp => Body::DelayResp {
                receive: Timestamp::read(&mut r)?,
                requesting: PortIdentity {
                    clock: r.array()?,
                    port: r.u16()?,
                },
            },
            MessageType::Announce => {
                let origin = Timestamp::read(&mut r)?;
                let utc_offset = i16::from_be_bytes(r.array()?);
                r.u8()?;
                let priority1 = r.u8()?;
                let quality = ClockQuality {
                    class: r.u8()?,
                    accuracy: r.u8()?,
                    variance: r.u16()?,
                };
                let priority2 = r.u8()?;
                let grandmaster = r.array()?;
                let steps_removed = r.u16()?;
                let time_source = r.u8()?;
                Body::Announce {
                    origin,
                    announce: Announce {
                        utc_offset,
                        priority1,
                        quality,
                        priority2,
                        grandmaster,
                        steps_removed,
                        time_source,
                    },
                }
            }
            _ => Body::Raw,
        };
        Ok(Self {
            msg_type,
            version,
            domain,
            flags,
            correction,
            source,
            sequence,
            log_interval,
            body,
        })
    }
}

/// Message domain without full decoding (byte 4).
pub fn peek_domain(buf: &[u8]) -> Option<u8> {
    at::<1>(buf, 4, "PTP").ok().map(|b| b[0])
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn sync_followup_announce_roundtrip() {
        let id = PortIdentity::from_mac([0x00, 0x1D, 0xC1, 0x12, 0x34, 0x56], 1);
        assert_eq!(id.clock, [0x00, 0x1D, 0xC1, 0xFF, 0xFE, 0x12, 0x34, 0x56]);
        let t = Timestamp::from_parts(1_760_000_000, 123_456_789);
        let mut sync = Message::new(
            MessageType::Sync,
            0,
            id,
            7,
            -3,
            Body::Timestamp(Timestamp(0)),
        );
        sync.flags = Message::FLAG_TWO_STEP;
        let raw = sync.encode();
        assert_eq!(raw.len(), 44);
        assert_eq!(raw[6] & 0x02, 0x02, "bit two-step");
        let fu = Message::new(MessageType::FollowUp, 0, id, 7, -3, Body::Timestamp(t));
        assert_eq!(Message::parse(&fu.encode()).unwrap(), fu);
        let ann = Message::new(
            MessageType::Announce,
            5,
            id,
            1,
            0,
            Body::Announce {
                origin: Timestamp(0),
                announce: Announce {
                    utc_offset: TAI_UTC_OFFSET,
                    priority1: 248,
                    quality: ClockQuality {
                        class: 248,
                        accuracy: 0xFE,
                        variance: 0xFFFF,
                    },
                    priority2: 248,
                    grandmaster: id.clock,
                    steps_removed: 0,
                    time_source: 0xA0,
                },
            },
        );
        let raw = ann.encode();
        assert_eq!((raw.len(), raw[47], &raw[53..61]), (64, 248, &id.clock[..]));
        assert_eq!(Message::parse(&raw).unwrap(), ann);
        assert_eq!(peek_domain(&raw), Some(5));
    }

    #[test]
    fn media_clock_matches_driver_formula() {
        let t = Timestamp::from_parts(1_760_000_000, 123_456_789);
        let expected = ((1_760_000_000u128 * 48_000 + 123_456_789 * 48_000 / 1_000_000_000)
            & 0xFFFF_FFFF) as u32;
        assert_eq!(t.media_clock(0), expected);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Message::parse(&[0x00, 0x01]).is_err());
        let id = PortIdentity::from_mac([0; 6], 1);
        let mut raw = Message::new(
            MessageType::FollowUp,
            0,
            id,
            1,
            0,
            Body::Timestamp(Timestamp(5)),
        )
        .encode();
        raw[40] = 0xFF; // nanoseconds >= 10^9
        assert!(Message::parse(&raw).is_err());
    }
}
