#!/usr/bin/env python3
"""Summarize captured Livewire control protocols: ADV (4000/4001), clock (7000), PTP (319/320).

    python3 lwdump.py capture.pcapng [--adv] [--clock] [--ptp] [--json]

Without options: all three. Used to investigate Q6 (clock), Q7 (PTP), Q8/Q9 (ADV, SAC).
"""
import argparse
import collections
import json
import struct
import sys

from advcodec import DecodeError, decode_datagram, summarize
from pcapio import udp_packets

PTP_TYPES = {0: "Sync", 1: "Delay_Req", 2: "Pdelay_Req", 3: "Pdelay_Resp", 8: "Follow_Up",
             9: "Delay_Resp", 0xA: "Pdelay_Resp_Follow_Up", 0xB: "Announce", 0xC: "Signaling", 0xD: "Management"}


def ptp_message(payload):
    if len(payload) < 34:
        return None
    mtype = payload[0] & 0x0F
    msg = {
        "type": PTP_TYPES.get(mtype, mtype), "version": payload[1] & 0x0F, "length": struct.unpack_from(">H", payload, 2)[0],
        "domain": payload[4], "flags": f"0x{struct.unpack_from('>H', payload, 6)[0]:04X}",
        "two_step": bool(payload[6] & 0x02), "clock_identity": payload[20:28].hex("-"),
        "port": struct.unpack_from(">H", payload, 28)[0], "seq": struct.unpack_from(">H", payload, 30)[0],
        "log_interval": struct.unpack_from(">b", payload, 33)[0],
    }
    if mtype in (0, 8, 0xB) and len(payload) >= 44:
        sec = int.from_bytes(payload[34:40], "big")
        ns = struct.unpack_from(">I", payload, 40)[0]
        msg["timestamp"] = f"{sec}.{ns:09d}"
        msg["media_clock_48k"] = (sec * 48000 + ns * 48000 // 1_000_000_000) & 0xFFFFFFFF
    if mtype == 0xB and len(payload) >= 64:
        msg.update(gm_priority1=payload[47], gm_class=payload[48], gm_accuracy=payload[49],
                   gm_priority2=payload[52], gm_identity=payload[53:61].hex("-"),
                   steps_removed=struct.unpack_from(">H", payload, 61)[0], time_source=payload[63])
    return msg


CLOCK_EXT_PROFILE = 0xFA1A
CLOCK_TYPES = {bytes.fromhex("0a00caba"): "A", bytes.fromhex("0b00caba"): "B"}


def clock_packet(payload):
    """Livewire clock packet (docs/protocol/04-clock.md): RTP + FA1A extension."""
    out = {"len": len(payload), "hex": payload.hex()}
    if len(payload) >= 12 and payload[0] >> 6 == 2:
        b0, b1, seq, ts, ssrc = struct.unpack_from(">BBHII", payload, 0)
        out.update(rtp=True, x=bool(b0 & 0x10), pt=b1 & 0x7F, seq=seq, ts=ts, ssrc=f"0x{ssrc:08X}")
    else:
        out["rtp"] = False
    if len(payload) >= 24:
        profile, words = struct.unpack_from(">HH", payload, 12)
        out["ext_ok"] = profile == CLOCK_EXT_PROFILE and words == 0x14
        out["ext_profile"] = f"0x{profile:04X}"
        out["ext_words"] = words
        out["clock_seq"] = struct.unpack_from(">I", payload, 16)[0]
        out["clock_type"] = CLOCK_TYPES.get(bytes(payload[20:24]), payload[20:24].hex())
    if len(payload) >= 30:
        out["master_id"] = payload[26:30].hex()
    return out


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("capture")
    parser.add_argument("--adv", action="store_true")
    parser.add_argument("--clock", action="store_true")
    parser.add_argument("--ptp", action="store_true")
    parser.add_argument("--json", action="store_true", help="one JSON line per packet")
    args = parser.parse_args(argv)
    every = not (args.adv or args.clock or args.ptp)
    stats = collections.Counter()
    clock_ts = {}
    for pkt in udp_packets(args.capture):
        rec = None
        if (every or args.adv) and (pkt.dport in (4000, 4001) or pkt.sport in (4000, 4001)):
            try:
                hdr, msg = decode_datagram(pkt.payload)
                rec = {"proto": "adv", "envelope": hdr, "msg": summarize(msg)}
                stats[f"adv:{rec['msg']['id']}:ADVT={rec['msg']['advt']}"] += 1
            except DecodeError as exc:
                rec = {"proto": "adv", "error": str(exc), "hex": pkt.payload[:64].hex()}
                stats["adv:undecoded"] += 1
        elif (every or args.clock) and pkt.dport == 7000:
            rec = {"proto": "lwclock", **clock_packet(pkt.payload)}
            prev = clock_ts.get(pkt.src)
            if prev and rec.get("rtp"):
                rec["dt_s"] = round(pkt.ts - prev[0], 6)
                rec["dts"] = (rec["ts"] - prev[1]) & 0xFFFFFFFF
                if "clock_seq" in rec and prev[2] is not None:
                    rec["dclock_seq"] = (rec["clock_seq"] - prev[2]) & 0xFFFFFFFF
                stats[f"lwclock:{pkt.src}:dts={rec['dts']}"] += 1
            if rec.get("rtp"):
                clock_ts[pkt.src] = (pkt.ts, rec["ts"], rec.get("clock_seq"))
            stats[f"lwclock:{pkt.src}:type={rec.get('clock_type', '?')}:ext_ok={rec.get('ext_ok')}"] += 1
        elif (every or args.ptp) and pkt.dport in (319, 320):
            msg = ptp_message(pkt.payload)
            if msg:
                rec = {"proto": "ptp", **msg}
                stats[f"ptp:{msg['type']}:dom{msg['domain']}"] += 1
        if rec is None:
            continue
        rec.update(t=round(pkt.ts, 6), src=pkt.src, dst=pkt.dst, sport=pkt.sport, dport=pkt.dport, ttl=pkt.ttl, tos=pkt.tos)
        if args.json:
            print(json.dumps(rec, ensure_ascii=False))
        else:
            body = {k: v for k, v in rec.items() if k not in ("t", "src", "dst", "sport", "dport", "hex")}
            print(f"{rec['t']:.6f} {rec['src']}:{rec['sport']} -> {rec['dst']}:{rec['dport']} {body}")
    print("# totals:", dict(stats), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
