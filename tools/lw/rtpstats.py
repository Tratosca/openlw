#!/usr/bin/env python3
"""Statistics for captured Livewire / AES67 RTP streams (Q2, Q3, Q4, Q5).

    python3 rtpstats.py capture.pcapng [--port 5004] [--json]

Per stream (src, dst, port): PT, SSRC (does SSRC equal destination IP?), payload size,
inferred samples per packet, sequence/timestamp steps, bitrate, arrival jitter
(RFC 3550), TOS, TTL, IP ID, zero UDP checksum, VLAN.
"""
import argparse
import collections
import ipaddress
import json
import struct
import sys

from lwchan import group_to_channel
from pcapio import udp_packets

RATE = 48000


def rtp_header(payload):
    if len(payload) < 12 or payload[0] >> 6 != 2:
        return None
    b0, b1, seq, ts, ssrc = struct.unpack_from(">BBHII", payload, 0)
    offset = 12 + 4 * (b0 & 0x0F)
    if b0 & 0x10:
        if len(payload) < offset + 4:
            return None
        offset += 4 + 4 * struct.unpack_from(">H", payload, offset + 2)[0]
    end = len(payload)
    if b0 & 0x20 and end:
        end -= payload[-1]
    return dict(pt=b1 & 0x7F, marker=b1 >> 7, seq=seq, ts=ts, ssrc=ssrc, offset=offset,
                payload_len=max(end - offset, 0), csrc=b0 & 0x0F, ext=bool(b0 & 0x10), pad=bool(b0 & 0x20))


class Flow:
    def __init__(self, key):
        self.key = key
        self.count = 0
        self.first = self.last = None
        self.pts = collections.Counter()
        self.ssrcs = collections.Counter()
        self.sizes = collections.Counter()
        self.dseq = collections.Counter()
        self.dts = collections.Counter()
        self.tos = collections.Counter()
        self.ttl = collections.Counter()
        self.ip_ids = collections.Counter()
        self.udp_csum_zero = 0
        self.vlans = collections.Counter()
        self.prev = None
        self.jitter = 0.0
        self.markers = 0

    def add(self, pkt, h):
        self.count += 1
        self.first = pkt.ts if self.first is None else self.first
        self.last = pkt.ts
        self.pts[h["pt"]] += 1
        self.ssrcs[h["ssrc"]] += 1
        self.sizes[h["payload_len"]] += 1
        self.tos[pkt.tos] += 1
        self.ttl[pkt.ttl] += 1
        self.ip_ids[pkt.ip_id] += 1
        self.udp_csum_zero += pkt.udp_checksum == 0
        self.vlans[pkt.vlan] += 1
        self.markers += h["marker"]
        if self.prev is not None:
            p_ts, p_h = self.prev
            self.dseq[(h["seq"] - p_h["seq"]) & 0xFFFF] += 1
            dts = (h["ts"] - p_h["ts"]) & 0xFFFFFFFF
            self.dts[dts] += 1
            transit_delta = (pkt.ts - p_ts) * RATE - dts
            self.jitter += (abs(transit_delta) - self.jitter) / 16
        self.prev = (pkt.ts, h)

    def report(self):
        src, dst, port = self.key
        duration = (self.last - self.first) if self.count > 1 else 0
        size = self.sizes.most_common(1)[0][0]
        chan = group_to_channel(dst)
        nchn = 8 if chan and chan[1] == "surround" else 2
        ssrc = self.ssrcs.most_common(1)[0][0]
        return {
            "src": src, "dst": dst, "port": port,
            "livewire_channel": chan[0] if chan else None, "livewire_kind": chan[1] if chan else None,
            "packets": self.count, "duration_s": round(duration, 3),
            "pkt_rate": round((self.count - 1) / duration, 1) if duration else None,
            "pt": dict(self.pts), "ssrc": f"0x{ssrc:08X}",
            "ssrc_equals_dst": ssrc == int(ipaddress.IPv4Address(dst)),
            "payload_sizes": dict(self.sizes),
            "samples_per_packet_L24": size / (3 * nchn), "assumed_channels": nchn,
            "dseq": dict(self.dseq.most_common(5)), "dts": dict(self.dts.most_common(5)),
            "jitter_samples": round(self.jitter, 2), "markers": self.markers,
            "tos": dict(self.tos), "ttl": dict(self.ttl),
            "ip_id_distinct": len(self.ip_ids), "ip_id_top": f"0x{self.ip_ids.most_common(1)[0][0]:04X}",
            "udp_checksum_zero": self.udp_csum_zero, "vlan": {str(k): v for k, v in self.vlans.items()},
        }


def analyze(path, ports):
    flows = {}
    for pkt in udp_packets(path):
        if pkt.dport not in ports:
            continue
        h = rtp_header(pkt.payload)
        if h is None:
            continue
        key = (pkt.src, pkt.dst, pkt.dport)
        flows.setdefault(key, Flow(key)).add(pkt, h)
    return [f.report() for f in flows.values()]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("capture")
    parser.add_argument("--port", type=int, action="append", help="RTP port(s) (default 5004)")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    reports = analyze(args.capture, set(args.port or [5004]))
    if args.json:
        print(json.dumps(reports, indent=2))
        return 0
    for r in reports:
        chan = f"channel {r['livewire_channel']} ({r['livewire_kind']})" if r["livewire_channel"] else "not a Livewire group"
        print(f"{r['src']} -> {r['dst']}:{r['port']} [{chan}] {r['packets']} pkts {r['pkt_rate']} pkt/s "
              f"PT={r['pt']} SSRC={r['ssrc']} (=dst:{r['ssrc_equals_dst']}) payload={r['payload_sizes']} "
              f"samples/pkt={r['samples_per_packet_L24']} dseq={r['dseq']} dts={r['dts']} jitter={r['jitter_samples']} samples "
              f"TOS={r['tos']} TTL={r['ttl']} csum0={r['udp_checksum_zero']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
