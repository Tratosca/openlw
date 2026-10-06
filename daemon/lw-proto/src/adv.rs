//! Livewire source advertisements (`docs/protocol/03-advertisement.md`).
//!
//! Format observed on the network and verified with Livewire devices (see specification).

use std::net::Ipv4Addr;

use crate::channel::{Channel, GroupKind};
use crate::tlv::{FourCc, TlvMsg, Value};
use crate::Error;

pub const NEST: FourCc = FourCc::new(b"NEST");
pub const INDI: FourCc = FourCc::new(b"INDI");
pub const READ: FourCc = FourCc::new(b"READ");
pub const ADVD: FourCc = FourCc::new(b"ADVD");
pub const PVER: FourCc = FourCc::new(b"PVER");
pub const ADVT: FourCc = FourCc::new(b"ADVT");
pub const TERM: FourCc = FourCc::new(b"TERM");
pub const ADVV: FourCc = FourCc::new(b"ADVV");
pub const HWID: FourCc = FourCc::new(b"HWID");
pub const INIP: FourCc = FourCc::new(b"INIP");
pub const UDPC: FourCc = FourCc::new(b"UDPC");
pub const NUMS: FourCc = FourCc::new(b"NUMS");
pub const ATRN: FourCc = FourCc::new(b"ATRN");
pub const PSID: FourCc = FourCc::new(b"PSID");
pub const SHAB: FourCc = FourCc::new(b"SHAB");
pub const FSID: FourCc = FourCc::new(b"FSID");
pub const FAST: FourCc = FourCc::new(b"FAST");
pub const FASM: FourCc = FourCc::new(b"FASM");
pub const BSID: FourCc = FourCc::new(b"BSID");
pub const BAST: FourCc = FourCc::new(b"BAST");
pub const BASM: FourCc = FourCc::new(b"BASM");
pub const LPID: FourCc = FourCc::new(b"LPID");
pub const STPL: FourCc = FourCc::new(b"STPL");
pub const PSNM: FourCc = FourCc::new(b"PSNM");
pub const LABL: FourCc = FourCc::new(b"LABL");

/// Maximum sources per terminal and per datagram.
pub const MAX_SOURCES: usize = 240;
pub const SOURCES_PER_PAGE: usize = 8;
pub const PROTOCOL_VERSION: u16 = 2;
pub const DEFAULT_CONTROL_PORT: u16 = 4000;

/// Advertised forward-stream type (`FAST`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvStreamType {
    StereoL24,
    StereoL16,
    Surround,
    Other(u8),
}

impl AdvStreamType {
    pub const fn code(self) -> u8 {
        match self {
            AdvStreamType::StereoL24 => 2,
            AdvStreamType::StereoL16 => 3,
            AdvStreamType::Surround => 4,
            AdvStreamType::Other(c) => c,
        }
    }

    pub const fn from_code(c: u8) -> Self {
        match c {
            2 => AdvStreamType::StereoL24,
            3 => AdvStreamType::StereoL16,
            4 => AdvStreamType::Surround,
            other => AdvStreamType::Other(other),
        }
    }
}

/// Terminal description (`TERM` block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub advv: u32,
    pub hwid: u16,
    pub ip: Ipv4Addr,
    pub control_port: u16,
    pub nums: u16,
    /// Hostname, present only in full advertisements (up to 32 bytes).
    pub name: Option<String>,
}

impl Terminal {
    /// Terminal with `HWID` = low 16 bits of IP.
    pub fn new(advv: u32, ip: Ipv4Addr, name: &str) -> Self {
        let hwid = (u32::from(ip) & 0xFFFF) as u16;
        Self {
            advv,
            hwid,
            ip,
            control_port: DEFAULT_CONTROL_PORT,
            nums: 0,
            name: Some(name.to_string()),
        }
    }
}

/// Advertised source (`S###` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Slot 1..=240.
    pub slot: u16,
    pub channel: u32,
    pub shareable: u8,
    pub stream: Ipv4Addr,
    pub stream_type: AdvStreamType,
    pub stream_mode: u8,
    pub backfeed: Ipv4Addr,
    pub backfeed_type: u8,
    pub backfeed_mode: u8,
    pub lpid: u32,
    pub stpl: u8,
    pub name: String,
    pub label: Option<String>,
}

impl Source {
    /// Stereo/surround source advertised on its channel (forward/return groups derived from channel).
    pub fn new(slot: u16, channel: Channel, name: &str, stream_type: AdvStreamType) -> Self {
        let kind = if stream_type == AdvStreamType::Surround {
            GroupKind::Surround
        } else {
            GroupKind::Stereo
        };
        Self {
            slot,
            channel: channel.get().into(),
            shareable: 0,
            stream: channel.group(kind),
            stream_type,
            stream_mode: 1,
            backfeed: channel.group(GroupKind::Backfeed),
            backfeed_type: 0,
            backfeed_mode: 1,
            lpid: channel.get().into(),
            stpl: 0,
            name: name.to_string(),
            label: None,
        }
    }

    fn to_msg(&self) -> TlvMsg {
        let mut m = TlvMsg::new(INDI)
            .with(PSID, Value::U32(self.channel))
            .with(SHAB, Value::U8(self.shareable))
            .with(FSID, Value::U32(self.stream.into()))
            .with(FAST, Value::U8(self.stream_type.code()))
            .with(FASM, Value::U8(self.stream_mode))
            .with(BSID, Value::U32(self.backfeed.into()))
            .with(BAST, Value::U8(self.backfeed_type))
            .with(BASM, Value::U8(self.backfeed_mode))
            .with(LPID, Value::U32(self.lpid))
            .with(STPL, Value::U8(self.stpl))
            .with(PSNM, Value::Str(fixed(&self.name, 16)));
        if let Some(label) = self.label.as_deref().filter(|l| !l.is_empty()) {
            m = m.with(LABL, Value::Str(fixed(label, 10)));
        }
        m
    }

    fn from_msg(slot: u16, m: &TlvMsg) -> Result<Self, Error> {
        let num = |tag| {
            m.number(tag)
                .ok_or(Error::Invalid("champ numérique absent dans une source"))
        };
        let byte = |tag| m.number(tag).map(|v| v as u8).unwrap_or(0);
        Ok(Self {
            slot,
            channel: num(PSID)? as u32,
            shareable: byte(SHAB),
            stream: Ipv4Addr::from(num(FSID)? as u32),
            stream_type: AdvStreamType::from_code(byte(FAST)),
            stream_mode: byte(FASM),
            backfeed: Ipv4Addr::from(m.number(BSID).unwrap_or(0) as u32),
            backfeed_type: byte(BAST),
            backfeed_mode: byte(BASM),
            lpid: m.number(LPID).unwrap_or(0) as u32,
            stpl: byte(STPL),
            name: m.get(PSNM).and_then(Value::as_text).unwrap_or_default(),
            label: m.get(LABL).and_then(Value::as_text),
        })
    }
}

/// One advertisement page (one datagram).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// `true`: full advertisement (`ADVT=1`); `false`: short (`ADVT=2`).
    pub full: bool,
    pub terminal: Terminal,
    pub sources: Vec<Source>,
}

impl Advertisement {
    pub fn to_msg(&self) -> Result<TlvMsg, Error> {
        if self.sources.len() > SOURCES_PER_PAGE {
            return Err(Error::TooLong("plus de 8 sources par datagramme"));
        }
        let t = &self.terminal;
        let mut term = TlvMsg::new(INDI)
            .with(ADVV, Value::U32(t.advv))
            .with(HWID, Value::U16(t.hwid))
            .with(INIP, Value::U32(t.ip.into()))
            .with(UDPC, Value::U16(t.control_port))
            .with(NUMS, Value::U16(t.nums));
        if self.full {
            if let Some(name) = &t.name {
                term = term.with(ATRN, Value::Str(fixed(name, 32)));
            }
        }
        let mut msg = TlvMsg::new(NEST)
            .with(PVER, Value::U16(PROTOCOL_VERSION))
            .with(ADVT, Value::U8(if self.full { 1 } else { 2 }))
            .with(TERM, Value::Msg(term));
        if self.full {
            for s in &self.sources {
                if s.slot == 0 || usize::from(s.slot) > MAX_SOURCES {
                    return Err(Error::Invalid("emplacement de source hors 1..240"));
                }
                msg = msg.with(slot_tag(s.slot), Value::Msg(s.to_msg()));
            }
        }
        Ok(msg)
    }

    pub fn from_msg(msg: &TlvMsg) -> Result<Self, Error> {
        if msg.id != NEST {
            return Err(Error::Invalid(
                "message d'annonce : identifiant différent de NEST",
            ));
        }
        let full = match msg.number(ADVT) {
            Some(1) => true,
            Some(2) => false,
            _ => return Err(Error::Invalid("ADVT absent ou inconnu")),
        };
        let term = msg.msg(TERM).ok_or(Error::Invalid("bloc TERM absent"))?;
        let terminal = Terminal {
            advv: term.number(ADVV).unwrap_or(0) as u32,
            hwid: term.number(HWID).unwrap_or(0) as u16,
            ip: Ipv4Addr::from(term.number(INIP).ok_or(Error::Invalid("INIP absent"))? as u32),
            control_port: term.number(UDPC).unwrap_or(DEFAULT_CONTROL_PORT.into()) as u16,
            nums: term.number(NUMS).unwrap_or(0) as u16,
            name: term.get(ATRN).and_then(Value::as_text),
        };
        let mut sources = Vec::new();
        for (tag, value) in &msg.items {
            if let (Some(slot), Value::Msg(m)) = (parse_slot_tag(*tag), value) {
                sources.push(Source::from_msg(slot, m)?);
            }
        }
        Ok(Self {
            full,
            terminal,
            sources,
        })
    }

    /// Full-advertisement pages: eight sources per datagram (at least one page).
    pub fn full_pages(terminal: &Terminal, sources: &[Source]) -> Vec<Self> {
        let mut terminal = terminal.clone();
        terminal.nums = sources.len().min(MAX_SOURCES) as u16;
        let chunks: Vec<&[Source]> = if sources.is_empty() {
            vec![&[]]
        } else {
            sources.chunks(SOURCES_PER_PAGE).collect()
        };
        chunks
            .into_iter()
            .map(|c| Self {
                full: true,
                terminal: terminal.clone(),
                sources: c.to_vec(),
            })
            .collect()
    }
}

/// Full-advertisement request:
/// `TlvMsg 'READ'` with `ADVD` u8 = 1, sent unicast to the terminal's `INIP:UDPC`.
pub fn full_info_request() -> TlvMsg {
    TlvMsg::new(READ).with(ADVD, Value::U8(1))
}

/// Slot `S###` tag.
pub fn slot_tag(slot: u16) -> FourCc {
    let d = |n: u16| b'0' + (n % 10) as u8;
    FourCc::new(&[b'S', d(slot / 100), d(slot / 10), d(slot)])
}

/// Slot from an `S` + three ASCII digits tag.
pub fn parse_slot_tag(tag: FourCc) -> Option<u16> {
    let [s, a, b, c] = tag.bytes();
    if s != b'S' || ![a, b, c].iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0'))
}

/// Fixed-length zero-padded ASCII string: Livewire devices display
/// bytes, not UTF-8. Accented Latin letters transliterated; other characters → `?`.
fn fixed(text: &str, size: usize) -> Vec<u8> {
    let mut v: Vec<u8> = text.chars().flat_map(ascii).take(size).collect();
    v.resize(size, 0);
    v
}

fn ascii(c: char) -> Vec<u8> {
    let s: &str = match c {
        c if c.is_ascii() && !c.is_ascii_control() => return vec![c as u8],
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "A",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'Í' | 'Ì' | 'Î' | 'Ï' => "I",
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => "O",
        'ú' | 'ù' | 'û' | 'ü' => "u",
        'Ú' | 'Ù' | 'Û' | 'Ü' => "U",
        'ç' => "c",
        'Ç' => "C",
        'ñ' => "n",
        'Ñ' => "N",
        'ÿ' | 'ý' => "y",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        'ß' => "ss",
        '’' | '‘' => "'",
        '–' | '—' => "-",
        _ => "?",
    };
    s.bytes().collect()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::envelope;

    #[test]
    fn names_are_ascii_strings_like_the_omnia() {
        assert_eq!(
            &fixed("Régie été — Studio Ça", 32)[..21],
            b"Regie ete - Studio Ca"
        );
        assert_eq!(fixed("Régie", 4), b"Regi");
        assert_eq!(fixed("A\u{1F3B5}", 3), b"A?\0");
        let src = Source::new(
            1,
            Channel::new(31).unwrap(),
            "Mac",
            AdvStreamType::StereoL24,
        );
        let msg = src.to_msg();
        assert!(
            matches!(msg.get(PSNM), Some(Value::Str(_))),
            "PSNM en type 3 (chaîne)"
        );
    }

    #[test]
    fn full_roundtrip() {
        let ch = Channel::new(4001).unwrap();
        let src = Source::new(1, ch, "MAC TEST", AdvStreamType::StereoL24);
        assert_eq!(src.stream, Ipv4Addr::new(239, 192, 15, 161));
        assert_eq!(src.backfeed, Ipv4Addr::new(239, 193, 15, 161));
        let pages = Advertisement::full_pages(
            &Terminal::new(3, Ipv4Addr::new(192, 168, 10, 20), "openlw-test"),
            std::slice::from_ref(&src),
        );
        assert_eq!(pages.len(), 1);
        let raw =
            envelope::encode(&envelope::Header::datagram(1), &pages[0].to_msg().unwrap()).unwrap();
        let (_, msg) = envelope::decode(&raw).unwrap();
        let back = Advertisement::from_msg(&msg).unwrap();
        assert_eq!(back.terminal.hwid, (10 << 8) | 20);
        assert_eq!(back.terminal.name.as_deref(), Some("openlw-test"));
        assert_eq!(back.sources, vec![src]);
    }

    #[test]
    fn full_info_request_bytes() {
        let raw = envelope::encode(&envelope::Header::datagram(1), &full_info_request()).unwrap();
        assert_eq!(&raw[..4], &[3, 0, 2, 7]);
        assert_eq!(&raw[16..], b"READ\x00\x01ADVD\x07\x01");
    }

    #[test]
    fn paging_and_slots() {
        let t = Terminal::new(1, Ipv4Addr::new(10, 0, 0, 1), "t");
        let sources: Vec<Source> = (1..=17)
            .map(|i| {
                Source::new(
                    i,
                    Channel::new(100 + i).unwrap(),
                    "x",
                    AdvStreamType::StereoL24,
                )
            })
            .collect();
        let pages = Advertisement::full_pages(&t, &sources);
        assert_eq!(
            pages.iter().map(|p| p.sources.len()).collect::<Vec<_>>(),
            vec![8, 8, 1]
        );
        assert!(pages.iter().all(|p| p.terminal.nums == 17));
        assert_eq!(slot_tag(7), FourCc::new(b"S007"));
        assert_eq!(parse_slot_tag(FourCc::new(b"S240")), Some(240));
        assert_eq!(parse_slot_tag(FourCc::new(b"SHAB")), None);
    }
}
