//! En-tête Envelope de 16 octets qui précède un TlvMsg (`docs/protocol/03-advertisement.md`).

use crate::bytes::Reader;
use crate::tlv::TlvMsg;
use crate::Error;

pub const HEADER_LEN: usize = 16;
pub const ENVELOPE_VERSION: u8 = 7;
pub const TLV_VERSION: u8 = 2;
pub const LAYER_TLV: u8 = 3;

/// Type de message Envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    /// Sans acquittement (annonces multicast).
    Datagram,
    /// Fiable : retransmis jusqu'à acquittement.
    Message,
    Ack,
    Nack,
    Other(u8),
}

impl MsgType {
    pub const fn code(self) -> u8 {
        match self {
            MsgType::Datagram => 0,
            MsgType::Message => b'M',
            MsgType::Ack => b'A',
            MsgType::Nack => b'N',
            MsgType::Other(c) => c,
        }
    }

    pub const fn from_code(c: u8) -> Self {
        match c {
            0 => MsgType::Datagram,
            b'M' => MsgType::Message,
            b'A' => MsgType::Ack,
            b'N' => MsgType::Nack,
            other => MsgType::Other(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub layer: u8,
    pub msg_type: MsgType,
    pub tlv_version: u8,
    pub sequence: u32,
    pub result_port: u16,
    pub lock_id: u16,
    pub lock_tid: u32,
}

impl Header {
    pub fn datagram(sequence: u32) -> Self {
        Self {
            layer: LAYER_TLV,
            msg_type: MsgType::Datagram,
            tlv_version: TLV_VERSION,
            sequence,
            result_port: 0,
            lock_id: 0,
            lock_tid: 0,
        }
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[
            self.layer,
            self.msg_type.code(),
            self.tlv_version,
            ENVELOPE_VERSION,
        ]);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.result_port.to_be_bytes());
        out.extend_from_slice(&self.lock_id.to_be_bytes());
        out.extend_from_slice(&self.lock_tid.to_be_bytes());
    }

    /// Décode et valide l'en-tête (version Envelope 7 exigée, comme `CEnvelopeLayer1Base::ProcessMessage`).
    pub fn parse(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf, "en-tête Envelope");
        let [layer, msg_type, tlv_version, version] = r.array()?;
        if version != ENVELOPE_VERSION {
            return Err(Error::Invalid("version Envelope différente de 7"));
        }
        Ok(Self {
            layer,
            msg_type: MsgType::from_code(msg_type),
            tlv_version,
            sequence: r.u32()?,
            result_port: r.u16()?,
            lock_id: r.u16()?,
            lock_tid: r.u32()?,
        })
    }
}

/// Datagramme complet : en-tête + TlvMsg.
pub fn encode(header: &Header, msg: &TlvMsg) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(HEADER_LEN + 256);
    header.write(&mut out);
    msg.encode_into(&mut out)?;
    Ok(out)
}

pub fn decode(buf: &[u8]) -> Result<(Header, TlvMsg), Error> {
    let header = Header::parse(buf)?;
    let body = buf.get(HEADER_LEN..).ok_or(Error::Truncated {
        what: "datagramme Envelope",
        need: HEADER_LEN,
        have: buf.len(),
    })?;
    Ok((header, TlvMsg::decode(body)?))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::tlv::FourCc;

    #[test]
    fn layout() {
        let raw = encode(
            &Header::datagram(0x0102_0304),
            &TlvMsg::new(FourCc::new(b"NEST")),
        )
        .unwrap();
        assert_eq!(&raw[..8], &[3, 0, 2, 7, 1, 2, 3, 4]);
        assert_eq!(raw.len(), HEADER_LEN + 6);
        let (h, m) = decode(&raw).unwrap();
        assert_eq!(
            (h.sequence, h.msg_type, m.id),
            (0x0102_0304, MsgType::Datagram, FourCc::new(b"NEST"))
        );
        let mut bad = raw.clone();
        bad[3] = 6;
        assert!(decode(&bad).is_err());
    }
}
