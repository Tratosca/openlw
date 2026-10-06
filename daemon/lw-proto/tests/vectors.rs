//! Contrat : les vecteurs de `docs/protocol/vectors/` (générés par
//! `tools/lw/make_vectors.py`, implémentation Python indépendante) doivent être relus à l'identique.

#![allow(clippy::indexing_slicing)]

use std::net::Ipv4Addr;
use std::path::PathBuf;

use lw_proto::adv::{AdvStreamType, Advertisement};
use lw_proto::channel::{Channel, GroupKind};
use lw_proto::lwclock::{ClockKind, ClockPacket};
use lw_proto::rtp::Packet;
use lw_proto::sdp::Session;
use lw_proto::{envelope, tlv};
use serde_json::Value;

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/vectors")
}

fn load(name: &str) -> Value {
    let text =
        std::fs::read_to_string(vectors().join(name)).unwrap_or_else(|e| panic!("{name} : {e}"));
    serde_json::from_str(&text).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn chan2mcast() {
    let v = load("chan2mcast.json");
    for row in v["valid"].as_array().unwrap() {
        let ch = Channel::new(row["channel"].as_u64().unwrap() as u16).unwrap();
        let kind = match row["kind"].as_str().unwrap() {
            "stereo" => GroupKind::Stereo,
            "backfeed" => GroupKind::Backfeed,
            "surround" => GroupKind::Surround,
            k => panic!("type {k}"),
        };
        let group: Ipv4Addr = row["group"].as_str().unwrap().parse().unwrap();
        assert_eq!(ch.group(kind), group);
        assert_eq!(Channel::from_group(group), Some((ch, kind)));
    }
    for bad in v["invalid_channels"].as_array().unwrap() {
        assert!(Channel::new(bad.as_u64().unwrap() as u16).is_none());
    }
}

#[test]
fn adv_packets() {
    let v = load("adv_packets.json");
    for pkt in v["packets"].as_array().unwrap() {
        let raw = hex(pkt["hex"].as_str().unwrap());
        let (header, msg) = envelope::decode(&raw).unwrap();
        assert_eq!(u64::from(header.sequence), pkt["seq"].as_u64().unwrap());
        // Ré-encodage octet pour octet : même sérialisation que l'implémentation Python.
        assert_eq!(envelope::encode(&header, &msg).unwrap(), raw);
        let adv = Advertisement::from_msg(&msg).unwrap();
        let expect = &pkt["expect"];
        assert_eq!(adv.full, expect["ADVT"] == 1);
        assert_eq!(
            u64::from(u32::from(adv.terminal.ip)),
            expect["TERM"]["INIP"].as_u64().unwrap()
        );
        assert_eq!(
            adv.terminal.name.as_deref(),
            expect["TERM"]["ATRN"].as_str()
        );
        for src in &adv.sources {
            let e = &expect[format!("S{:03}", src.slot)];
            assert_eq!(u64::from(src.channel), e["PSID"].as_u64().unwrap());
            assert_eq!(src.name, e["PSNM"].as_str().unwrap());
            assert_eq!(
                u64::from(u32::from(src.stream)),
                e["FSID"].as_u64().unwrap()
            );
            assert_eq!(
                u64::from(src.stream_type.code()),
                e["FAST"].as_u64().unwrap()
            );
        }
        // Reconstruction depuis la structure typée : identique au vecteur.
        assert_eq!(
            envelope::encode(&header, &adv.to_msg().unwrap()).unwrap(),
            raw
        );
        assert!(tlv::TlvMsg::decode(&raw[envelope::HEADER_LEN..]).is_ok());
    }
    let first = Advertisement::from_msg(
        &envelope::decode(&hex(v["packets"][0]["hex"].as_str().unwrap()))
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(first.sources[1].stream_type, AdvStreamType::Surround);
}

#[test]
fn rtp_headers() {
    let v = load("rtp_headers.json");
    for s in v["streams"].as_array().unwrap() {
        let header = hex(s["header_hex"].as_str().unwrap());
        let pkt = Packet::parse(&header).unwrap();
        let group: Ipv4Addr = s["group"].as_str().unwrap().parse().unwrap();
        assert_eq!(pkt.ssrc, u32::from(group));
        assert_eq!(pkt.payload_type, 96);
    }
}

#[test]
fn lwclock() {
    let v = load("lwclock.json");
    let mut prev: Option<ClockPacket> = None;
    for p in v["packets"].as_array().unwrap() {
        let raw = hex(p["hex"].as_str().unwrap());
        assert_eq!(raw.len() as u64, v["expected_len"].as_u64().unwrap());
        let c = ClockPacket::parse(&raw).unwrap();
        assert_eq!(c.kind, ClockKind::A);
        assert_eq!(
            u64::from(c.clock_sequence),
            p["clock_seq"].as_u64().unwrap()
        );
        assert_eq!(u64::from(c.timestamp), p["rtp_ts"].as_u64().unwrap());
        if let Some(prev) = prev {
            assert_eq!(c.timestamp.wrapping_sub(prev.timestamp), 12);
        }
        prev = Some(c);
    }
}

#[test]
fn sdp_reference() {
    let text = std::fs::read_to_string(vectors().join("sdp/aes67-ch101.sdp")).unwrap();
    let s = Session::parse(&text).unwrap();
    assert_eq!(
        (
            s.group,
            s.port,
            s.payload_type,
            s.bits,
            s.channels,
            s.samples_per_packet
        ),
        (Ipv4Addr::new(239, 192, 0, 101), 5004, 96, 24, 2, 48)
    );
    assert_eq!(s.ptp.unwrap().domain, 0);
    assert_eq!(
        s.to_text(),
        text,
        "régénération identique au gabarit Python"
    );
}
