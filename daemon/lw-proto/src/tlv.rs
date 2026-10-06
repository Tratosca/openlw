//! TlvMsg: typed TLV message for Livewire advertisements (`docs/protocol/03-advertisement.md`).
//!
//! `u32 id (FourCC) | u16 count | count × (u32 tag | u8 type | value)`, all big-endian.

use std::fmt;

use crate::bytes::Reader;
use crate::Error;

/// Maximum nesting depth accepted by decoding.
pub const MAX_DEPTH: usize = 8;

/// Four-character identifier or tag.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCc(pub u32);

impl FourCc {
    /// Construct a FourCC from four ASCII bytes (evaluated at compile time).
    pub const fn new(s: &[u8; 4]) -> Self {
        Self(u32::from_be_bytes(*s))
    }

    pub fn bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.bytes();
        if b.iter().all(|c| (0x20..0x7F).contains(c)) {
            b.iter().try_for_each(|&c| write!(f, "{}", char::from(c)))
        } else {
            write!(f, "0x{:08X}", self.0)
        }
    }
}

/// Item value, with wire type code in parentheses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// (1)
    U32(u32),
    /// (2)
    Bytes(Vec<u8>),
    /// (3)
    Str(Vec<u8>),
    /// (4)
    Words(Vec<u16>),
    /// (5)
    Dwords(Vec<u32>),
    /// (6)
    Msg(TlvMsg),
    /// (7)
    U8(u8),
    /// (8)
    U16(u16),
    /// (9)
    U64(u64),
}

impl Value {
    pub const fn type_code(&self) -> u8 {
        match self {
            Value::U32(_) => 1,
            Value::Bytes(_) => 2,
            Value::Str(_) => 3,
            Value::Words(_) => 4,
            Value::Dwords(_) => 5,
            Value::Msg(_) => 6,
            Value::U8(_) => 7,
            Value::U16(_) => 8,
            Value::U64(_) => 9,
        }
    }

    /// Integer value regardless of width.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::U8(v) => Some(v.into()),
            Value::U16(v) => Some(v.into()),
            Value::U32(v) => Some(v.into()),
            Value::U64(v) => Some(v),
            _ => None,
        }
    }

    /// String from a byte array, cut at first NUL.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Bytes(b) | Value::Str(b) => {
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Some(b.iter().take(end).map(|&c| char::from(c)).collect())
            }
            _ => None,
        }
    }
}

/// TlvMsg message: identifier and ordered items (tags may repeat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlvMsg {
    pub id: FourCc,
    pub items: Vec<(FourCc, Value)>,
}

impl TlvMsg {
    pub fn new(id: FourCc) -> Self {
        Self {
            id,
            items: Vec::new(),
        }
    }

    pub fn with(mut self, tag: FourCc, value: Value) -> Self {
        self.items.push((tag, value));
        self
    }

    pub fn get(&self, tag: FourCc) -> Option<&Value> {
        self.items.iter().find(|(t, _)| *t == tag).map(|(_, v)| v)
    }

    pub fn number(&self, tag: FourCc) -> Option<u64> {
        self.get(tag).and_then(Value::as_u64)
    }

    pub fn msg(&self, tag: FourCc) -> Option<&TlvMsg> {
        match self.get(tag) {
            Some(Value::Msg(m)) => Some(m),
            _ => None,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let start = out.len();
        out.extend_from_slice(&self.id.0.to_be_bytes());
        let count =
            u16::try_from(self.items.len()).map_err(|_| Error::TooLong("nombre d'items TlvMsg"))?;
        out.extend_from_slice(&count.to_be_bytes());
        for (tag, value) in &self.items {
            out.extend_from_slice(&tag.0.to_be_bytes());
            out.push(value.type_code());
            match value {
                Value::U8(v) => out.push(*v),
                Value::U16(v) => out.extend_from_slice(&v.to_be_bytes()),
                Value::U32(v) => out.extend_from_slice(&v.to_be_bytes()),
                Value::U64(v) => out.extend_from_slice(&v.to_be_bytes()),
                Value::Bytes(b) | Value::Str(b) => {
                    out.extend_from_slice(&len16(b.len())?.to_be_bytes());
                    out.extend_from_slice(b);
                }
                Value::Words(w) => {
                    out.extend_from_slice(&len16(w.len())?.to_be_bytes());
                    w.iter()
                        .for_each(|v| out.extend_from_slice(&v.to_be_bytes()));
                }
                Value::Dwords(d) => {
                    out.extend_from_slice(&len16(d.len())?.to_be_bytes());
                    d.iter()
                        .for_each(|v| out.extend_from_slice(&v.to_be_bytes()));
                }
                Value::Msg(m) => {
                    let body = m.encode()?;
                    out.extend_from_slice(&len16(body.len())?.to_be_bytes());
                    out.extend_from_slice(&body);
                }
            }
        }
        if out.len() - start > usize::from(u16::MAX) {
            return Err(Error::TooLong("TlvMsg"));
        }
        Ok(())
    }

    /// Decode a complete message (extra bytes tolerated).
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        Self::decode_at_depth(buf, 0)
    }

    fn decode_at_depth(buf: &[u8], depth: usize) -> Result<Self, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::Invalid("imbrication TlvMsg trop profonde"));
        }
        let mut r = Reader::new(buf, "TlvMsg");
        let id = FourCc(r.u32()?);
        let count = r.u16()?;
        // Each item occupies at least six bytes: bound allocations for hostile input.
        let mut items = Vec::with_capacity(usize::from(count).min(r.remaining() / 6));
        for _ in 0..count {
            let tag = FourCc(r.u32()?);
            let value = match r.u8()? {
                1 => Value::U32(r.u32()?),
                7 => Value::U8(r.u8()?),
                8 => Value::U16(r.u16()?),
                9 => Value::U64(r.u64()?),
                2 => {
                    let n = usize::from(r.u16()?);
                    Value::Bytes(r.take(n)?.to_vec())
                }
                3 => {
                    let n = usize::from(r.u16()?);
                    Value::Str(r.take(n)?.to_vec())
                }
                4 => {
                    let n = usize::from(r.u16()?);
                    let raw = r.take(n * 2)?;
                    Value::Words(
                        raw.chunks_exact(2)
                            .filter_map(|c| c.try_into().ok())
                            .map(u16::from_be_bytes)
                            .collect(),
                    )
                }
                5 => {
                    let n = usize::from(r.u16()?);
                    let raw = r.take(n * 4)?;
                    Value::Dwords(
                        raw.chunks_exact(4)
                            .filter_map(|c| c.try_into().ok())
                            .map(u32::from_be_bytes)
                            .collect(),
                    )
                }
                6 => {
                    let n = usize::from(r.u16()?);
                    Value::Msg(Self::decode_at_depth(r.take(n)?, depth + 1)?)
                }
                _ => return Err(Error::Invalid("type d'item TlvMsg inconnu")),
            };
            items.push((tag, value));
        }
        Ok(Self { id, items })
    }
}

fn len16(n: usize) -> Result<u16, Error> {
    u16::try_from(n).map_err(|_| Error::TooLong("valeur TlvMsg"))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn wire_layout() {
        let m = TlvMsg::new(FourCc::new(b"NEST")).with(FourCc::new(b"PVER"), Value::U16(2));
        assert_eq!(m.encode().unwrap(), b"NEST\x00\x01PVER\x08\x00\x02");
    }

    #[test]
    fn roundtrip_all_types() {
        let inner = TlvMsg::new(FourCc::new(b"INDI")).with(FourCc::new(b"PSID"), Value::U32(101));
        let m = TlvMsg::new(FourCc::new(b"TEST"))
            .with(FourCc::new(b"A___"), Value::U32(0xDEAD_BEEF))
            .with(FourCc::new(b"B___"), Value::Bytes(b"abc\0".to_vec()))
            .with(FourCc::new(b"C___"), Value::Str(b"hello".to_vec()))
            .with(FourCc::new(b"D___"), Value::Words(vec![1, 2, 3]))
            .with(FourCc::new(b"E___"), Value::Dwords(vec![4, 5]))
            .with(FourCc::new(b"F___"), Value::Msg(inner))
            .with(FourCc::new(b"G___"), Value::U8(7))
            .with(FourCc::new(b"H___"), Value::U16(4000))
            .with(FourCc::new(b"I___"), Value::U64(0x0102_0304_0506_0708));
        let raw = m.encode().unwrap();
        let back = TlvMsg::decode(&raw).unwrap();
        assert_eq!(back, m);
        assert_eq!(
            back.msg(FourCc::new(b"F___"))
                .unwrap()
                .number(FourCc::new(b"PSID")),
            Some(101)
        );
        assert_eq!(
            back.get(FourCc::new(b"B___")).unwrap().as_text().as_deref(),
            Some("abc")
        );
        for cut in 0..raw.len() {
            assert!(TlvMsg::decode(&raw[..cut]).is_err(), "coupure à {cut}");
        }
    }

    #[test]
    fn depth_limit() {
        let mut m = TlvMsg::new(FourCc::new(b"LEAF"));
        for _ in 0..=MAX_DEPTH {
            m = TlvMsg::new(FourCc::new(b"NEST")).with(FourCc::new(b"NEST"), Value::Msg(m));
        }
        assert!(TlvMsg::decode(&m.encode().unwrap()).is_err());
    }
}
