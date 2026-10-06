//! Decoder robustness against hostile input (`cargo-fuzz` substitute on stable).
//!
//! Random inputs and mutations of valid packets: no decoder may panic.
//! Deterministic generator (xorshift): failures are reproducible.

#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;

use lw_proto::adv::{AdvStreamType, Advertisement, Source, Terminal};
use lw_proto::channel::Channel;
use lw_proto::envelope;
use lw_proto::lwclock::ClockPacket;
use lw_proto::ptp;
use lw_proto::rtp::Packet;
use lw_proto::sdp::Session;
use lw_proto::tlv::TlvMsg;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn decode_everything(buf: &[u8]) {
    let _ = TlvMsg::decode(buf);
    if let Ok((_, msg)) = envelope::decode(buf) {
        let _ = Advertisement::from_msg(&msg);
    }
    let _ = Packet::parse(buf);
    let _ = ClockPacket::parse(buf);
    let _ = ptp::Message::parse(buf);
    let _ = Session::parse(&String::from_utf8_lossy(buf));
}

fn seeds() -> Vec<Vec<u8>> {
    let t = Terminal::new(9, Ipv4Addr::new(192, 168, 10, 20), "seed");
    let sources: Vec<Source> = (1..=3)
        .map(|i| {
            Source::new(
                i,
                Channel::new(4000 + i).unwrap(),
                "SEED",
                AdvStreamType::StereoL24,
            )
        })
        .collect();
    let adv = &Advertisement::full_pages(&t, &sources)[0];
    let id = ptp::PortIdentity::from_mac([2, 0, 0, 0, 0, 1], 1);
    let mut out = vec![
        envelope::encode(&envelope::Header::datagram(1), &adv.to_msg().unwrap()).unwrap(),
        ptp::Message::new(
            ptp::MessageType::FollowUp,
            0,
            id,
            1,
            -3,
            ptp::Body::Timestamp(ptp::Timestamp(1)),
        )
        .encode(),
    ];
    let mut clock = vec![
        0x90, 96, 0, 1, 0, 0, 0xBB, 0x80, 0xEF, 0xC0, 0xFF, 0x02, 0xFA, 0x1A, 0x00, 0x14,
    ];
    clock.extend_from_slice(&[0; 80]);
    out.push(clock);
    out.push(b"v=0\r\no=- 0 0 IN IP4 10.0.0.1\r\ns=x\r\nc=IN IP4 239.192.0.1/32\r\nt=0 0\r\nm=audio 5004 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\na=ptime:1\r\n".to_vec());
    out
}

#[test]
fn random_inputs_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let len = rng.below(300);
        decode_everything(&rng.bytes(len));
    }
}

#[test]
fn mutated_valid_packets_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for seed in seeds() {
        for _ in 0..5_000 {
            let mut buf = seed.clone();
            for _ in 0..=rng.below(6) {
                match rng.below(4) {
                    0 if !buf.is_empty() => {
                        let i = rng.below(buf.len());
                        buf[i] = rng.next() as u8;
                    }
                    1 if !buf.is_empty() => buf.truncate(rng.below(buf.len())),
                    2 => {
                        let i = rng.below(buf.len() + 1);
                        buf.insert(i, rng.next() as u8);
                    }
                    _ if buf.len() > 2 => {
                        // Hostile u16 lengths.
                        let i = rng.below(buf.len() - 1);
                        buf[i] = 0xFF;
                        buf[i + 1] = 0xFF;
                    }
                    _ => {}
                }
            }
            decode_everything(&buf);
        }
    }
}

#[test]
fn hostile_count_does_not_allocate_wildly() {
    // count = 65535 in a six-byte message: fast rejection, no huge allocation.
    assert!(TlvMsg::decode(b"NEST\xFF\xFF").is_err());
}
