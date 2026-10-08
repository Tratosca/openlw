#!/usr/bin/env python3
"""Advertise a mock Livewire source (ADV), based on docs/protocol/03-advertisement.md.

    python3 emit_adv.py --iface en7 --channel 4001 --name "MAC TEST" [--seconds 120] [--dry-run]

Acceptance criterion: source appears in Livewire device source browsers.
Combine with emit_rtp.py for audio.
Timing: full advertisement at startup and after 1 s, then short every 20 s +/- 5 s,
full after eight short advertisements.
"""
import argparse
import random
import sys
import time

from advcodec import advertisement, encode_datagram, source_block

ADV_GROUP = "239.192.255.3"
ADV_PORT = 4001


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--iface")
    parser.add_argument("--ip", help="IP to advertise (default: the interface address)")
    parser.add_argument("--channel", type=int, required=True)
    parser.add_argument("--name", default="LWRE TEST")
    parser.add_argument("--terminal", default="openlw-test")
    parser.add_argument("--fast", type=int, default=2, help="2 stereo L24, 3 stereo L16, 4 surround")
    parser.add_argument("--seconds", type=float, default=120)
    parser.add_argument("--advv", type=int, default=1)
    parser.add_argument("--dry-run", action="store_true", help="print the datagram in hexadecimal without sending")
    args = parser.parse_args(argv)

    ip = args.ip
    sock = None
    if not args.dry_run:
        from netiface import udp_socket
        # --ip changes only advertised address (INIP): useful to simulate another terminal in the lab.
        sock, iface_ip = udp_socket(args.iface, tos=0)
        ip = args.ip or iface_ip
    ip = ip or "192.168.0.10"
    sources = [(1, source_block(args.channel, args.name, fast=args.fast))]
    full = encode_datagram(advertisement(args.advv, ip, sources, name=args.terminal, full=True), seq=1)
    if args.dry_run:
        print(full.hex())
        return 0
    seq = 1
    shorts = 0
    deadline = time.monotonic() + args.seconds
    next_send, send_full = time.monotonic(), True
    first_cycle = True
    while time.monotonic() < deadline:
        time.sleep(max(0.0, next_send - time.monotonic()))
        msg = advertisement(args.advv, ip, sources, name=args.terminal, full=send_full)
        sock.sendto(encode_datagram(msg, seq), (ADV_GROUP, ADV_PORT))
        print(f"{time.strftime('%H:%M:%S')} {'full' if send_full else 'short'} seq={seq}")
        seq = (seq + 1) & 0xFFFFFFFF or 1  # Envelope sequence number is never zero
        if send_full and first_cycle:
            next_send, first_cycle, send_full = time.monotonic() + 1 + random.uniform(-0.5, 0.5), False, True
            continue
        shorts = 0 if send_full else shorts + 1
        send_full = shorts >= 8
        next_send = time.monotonic() + 20 + random.uniform(-5, 5)
    return 0


if __name__ == "__main__":
    sys.exit(main())
