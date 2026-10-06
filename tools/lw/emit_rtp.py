#!/usr/bin/env python3
"""Test Livewire RTP transmitter (Standard 240, AES67 48, surround 60 samples), based on documentation.

    python3 emit_rtp.py --iface en7 --channel 4001 --mode standard --seconds 30 [--freq 997]

Acceptance criterion (Q2): stream plays on a Livewire receiver
(Statistics window without errors). Software pacing: regularity suffices for interoperability tests,
not for production.
"""
import argparse
import math
import struct
import sys
import time

from lwchan import channel_to_group
from netiface import udp_socket

MODES = {  # (samples per packet, channels, group type)
    "standard": (240, 2, "stereo"),
    "aes67": (48, 2, "stereo"),
    "livestream": (12, 2, "stereo"),
    "surround": (60, 8, "surround"),
}
RATE = 48000


def l24(value):
    v = max(-8388608, min(8388607, int(value)))
    return (v & 0xFFFFFF).to_bytes(3, "big")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--iface", required=True)
    parser.add_argument("--channel", type=int, required=True)
    parser.add_argument("--mode", choices=sorted(MODES), default="standard")
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--freq", type=float, default=997.0)
    parser.add_argument("--level-dbfs", type=float, default=-20.0)
    parser.add_argument("--pt", type=int, default=96)
    parser.add_argument("--ssrc", type=lambda x: int(x, 0), help="defaut : adresse IP du groupe ")
    parser.add_argument("--port", type=int, default=5004)
    parser.add_argument("--tos", type=lambda x: int(x, 0), default=0xB8)
    args = parser.parse_args(argv)

    samples, nchn, kind = MODES[args.mode]
    group = channel_to_group(args.channel, kind)
    ssrc = args.ssrc if args.ssrc is not None else struct.unpack(">I", bytes(int(x) for x in group.split(".")))[0]
    sock, ip = udp_socket(args.iface, tos=args.tos, bind_port=args.port)
    amp = 8388607 * 10 ** (args.level_dbfs / 20)
    period = samples / RATE
    seq, ts, n = 0, 0, 0
    total = int(args.seconds / period)
    print(f"{args.mode} canal {args.channel} -> {group}:{args.port} depuis {ip}, {samples} ech/pkt, {1/period:.0f} pkt/s, SSRC 0x{ssrc:08X}")
    start = time.perf_counter()
    for k in range(total):
        frames = bytearray()
        for i in range(samples):
            s = amp * math.sin(2 * math.pi * args.freq * (n + i) / RATE)
            for c in range(nchn):
                frames += l24(s if c % 2 == 0 else -s)
        header = struct.pack(">BBHII", 0x80, args.pt & 0x7F, seq, ts, ssrc)
        sock.sendto(header + frames, (group, args.port))
        seq = (seq + 1) & 0xFFFF
        ts = (ts + samples) & 0xFFFFFFFF
        n += samples
        delay = start + (k + 1) * period - time.perf_counter()
        if delay > 0:
            time.sleep(delay)
    print(f"{total} paquets emis")
    return 0


if __name__ == "__main__":
    sys.exit(main())
