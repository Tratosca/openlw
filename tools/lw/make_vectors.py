#!/usr/bin/env python3
"""Genere docs/protocol/vectors/ a partir de la doc (contrat avec src/endpoint).

    python3 tools/lw/make_vectors.py [--out docs/protocol/vectors]

Les vecteurs sont construits d'apres la specification ; ceux qui portent "source": "doc"
devront etre completes par des paquets reels ("source": "pcap:<ID>") apres les captures.
"""
import argparse
import json
import pathlib
import struct
import sys

from advcodec import advertisement, encode_datagram, source_block
from lwchan import channel_to_group
from sdptool import generate

CHANNELS = [1, 2, 101, 255, 256, 257, 4001, 4095, 16384, 32766]


def chan2mcast():
    rows = []
    for c in CHANNELS:
        for kind in ("stereo", "backfeed", "surround"):
            rows.append({"channel": c, "kind": kind, "group": channel_to_group(c, kind)})
    invalid = [0, 0x7FFF, 0x8000, 65535]
    return {"source": "doc:01-channels", "valid": rows, "invalid_channels": invalid}


def adv_packets():
    ip = "192.168.10.20"
    full = advertisement(7, ip, [(1, source_block(4001, "MAC TEST 1")), (2, source_block(4002, "MAC TEST 2", fast=4))],
                         name="openlw-test", full=True)
    short = advertisement(7, ip, [(1, source_block(4001, "MAC TEST 1"))], full=False)
    return {
        "source": "doc:03-advertisement",
        "packets": [
            {"name": "full_2_sources", "seq": 1, "hex": encode_datagram(full, 1).hex(), "expect": full.to_dict()},
            {"name": "short", "seq": 2, "hex": encode_datagram(short, 2).hex(), "expect": short.to_dict()},
        ],
    }


def rtp_headers():
    out = []
    for mode, samples, nchn, kind in (("standard", 240, 2, "stereo"), ("aes67", 48, 2, "stereo"),
                                      ("livestream", 12, 2, "stereo"), ("surround", 60, 8, "surround")):
        group = channel_to_group(101, kind)
        ssrc = struct.unpack(">I", bytes(int(x) for x in group.split(".")))[0]
        hdr = struct.pack(">BBHII", 0x80, 96, 1000, 480000, ssrc)
        out.append({"mode": mode, "group": group, "samples_per_packet": samples, "channels": nchn,
                    "payload_bytes_L24": samples * nchn * 3, "ts_step": samples, "packets_per_s": 48000 // samples,
                    "header_hex": hdr.hex(), "ssrc_is_dst": True,
                    "confidence": {"ssrc_is_dst": "choix OpenLW", "ts_step": "observe en Standard, suppose pour les autres formats"}})
    return {"source": "doc:02-rtp-audio", "streams": out}


def clock_packet(seq, ts, kind="A", master_id=bytes.fromhex("c0a80a14")):
    """Paquet d'horloge synthetique : RTP (X=1) + extension FA1A de 20 mots ; octets inconnus a zero."""
    ext = bytearray(80)
    struct.pack_into(">I", ext, 0, seq)
    ext[4:8] = bytes.fromhex("0a00caba" if kind == "A" else "0b00caba")
    ext[10:14] = master_id  # octets 26-29 de la charge UDP
    return struct.pack(">BBHII", 0x90, 96, seq & 0xFFFF, ts, 0xEFC0FF02) + struct.pack(">HH", 0xFA1A, 0x14) + bytes(ext)


def lwclock():
    pkts = [{"clock_seq": 1000 + i, "rtp_ts": 48000 + 12 * i, "type": "A",
             "hex": clock_packet(1000 + i, 48000 + 12 * i).hex()} for i in range(3)]
    return {"source": "doc:04-clock (synthetique, octets 24-95 inconnus a zero)",
            "expected_len": 96, "period_us": 250, "ts_step": 12, "packets": pkts}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default=str(pathlib.Path(__file__).resolve().parents[2] / "docs/protocol/vectors"))
    args = parser.parse_args(argv)
    out = pathlib.Path(args.out)
    (out / "sdp").mkdir(parents=True, exist_ok=True)
    for name, data in (("chan2mcast.json", chan2mcast()), ("adv_packets.json", adv_packets()), ("rtp_headers.json", rtp_headers()), ("lwclock.json", lwclock())):
        (out / name).write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    sdp = generate(channel_to_group(101), "192.168.10.20", "MAC TEST 1", pt=96, samples=48,
                   gm="00-1D-C1-FF-FE-12-34-56", domain=0)
    (out / "sdp" / "aes67-ch101.sdp").write_bytes(sdp.encode())
    print(f"vecteurs ecrits dans {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
