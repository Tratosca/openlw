#!/usr/bin/env python3
"""Grandmaster PTPv2 minimal pour le banc (horodatage logiciel, two-step, multicast E2E).

    sudo python3 ptp_gm.py --iface en7 [--domain 0] [--sync-log -3] [--priority1 248]

Couvre le minimum d'un esclave PTP logiciel : Sync + Follow_Up
+ Announce sur 224.0.1.129, plus Delay_Resp pour des esclaves E2E (linuxptp).
Pas de BMCA : ne pas lancer sur un reseau ou un vrai grandmaster existe dans le meme domaine.
Horloge = horloge systeme (UTC) + offset TAI ; precision de l'ordre de la dizaine de microsecondes,
suffisante pour la RE, pas pour l'exploitation.
"""
import argparse
import select
import socket
import struct
import sys
import time

PTP_GROUP = "224.0.1.129"
EVENT_PORT, GENERAL_PORT = 319, 320
SYNC, DELAY_REQ, FOLLOW_UP, DELAY_RESP, ANNOUNCE = 0x0, 0x1, 0x8, 0x9, 0xB
CONTROL = {SYNC: 0, DELAY_REQ: 1, FOLLOW_UP: 2, DELAY_RESP: 3, ANNOUNCE: 5}
LENGTH = {SYNC: 44, DELAY_REQ: 44, FOLLOW_UP: 44, DELAY_RESP: 54, ANNOUNCE: 64}
TAI_UTC_OFFSET = 37  # secondes (depuis 2017)


def timestamp(ns):
    """Timestamp PTP sur 10 octets : secondes sur 48 bits, nanosecondes sur 32 bits."""
    sec, nsec = divmod(ns, 1_000_000_000)
    return struct.pack(">HII", (sec >> 32) & 0xFFFF, sec & 0xFFFFFFFF, nsec)


def parse_timestamp(raw):
    hi, lo, nsec = struct.unpack(">HII", raw)
    return ((hi << 32) | lo) * 1_000_000_000 + nsec


def header(mtype, domain, clock_id, seq, log_interval, flags=0, port=1):
    return struct.pack(">BBHBBHq4s8sHHBb", mtype & 0x0F, 2, LENGTH[mtype], domain, 0, flags, 0, b"\0" * 4,
                       clock_id, port, seq & 0xFFFF, CONTROL[mtype], log_interval)


def sync(domain, clock_id, seq, log_interval, origin_ns=0):
    return header(SYNC, domain, clock_id, seq, log_interval, flags=0x0200) + timestamp(origin_ns)


def follow_up(domain, clock_id, seq, log_interval, precise_ns):
    return header(FOLLOW_UP, domain, clock_id, seq, log_interval) + timestamp(precise_ns)


def announce(domain, clock_id, seq, log_interval, priority1=248, priority2=248, clock_class=248,
             accuracy=0xFE, variance=0xFFFF, time_source=0xA0):
    # flags : ptpTimescale (0x08) | currentUtcOffsetValid (0x04) dans l'octet de poids faible
    body = timestamp(0) + struct.pack(">hBBBBHB8sHB", TAI_UTC_OFFSET, 0, priority1, clock_class, accuracy,
                                      variance, priority2, clock_id, 0, time_source)
    return header(ANNOUNCE, domain, clock_id, seq, log_interval, flags=0x000C) + body


def delay_resp(domain, clock_id, req, receive_ns, log_interval):
    seq = struct.unpack_from(">H", req, 30)[0]
    requesting = req[20:30]
    return header(DELAY_RESP, domain, clock_id, seq, log_interval) + timestamp(receive_ns) + requesting


def clock_identity(mac):
    """EUI-64 derivee d'une MAC (insertion FF FE)."""
    return mac[:3] + b"\xff\xfe" + mac[3:]


def interface_mac(name):
    import re
    import subprocess
    out = subprocess.run(["ifconfig", name], capture_output=True, text=True).stdout
    m = re.search(r"ether ([0-9a-f:]{17})", out)
    return bytes.fromhex(m.group(1).replace(":", "")) if m else b"\x02\x00\x00\x00\x00\x01"


def now_tai_ns():
    return time.time_ns() + TAI_UTC_OFFSET * 1_000_000_000


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--iface", required=True)
    parser.add_argument("--domain", type=int, default=0)
    parser.add_argument("--sync-log", type=int, default=-3, help="log2 de l'intervalle Sync (-3 = 125 ms)")
    parser.add_argument("--announce-log", type=int, default=0, help="log2 de l'intervalle Announce (0 = 1 s)")
    parser.add_argument("--priority1", type=int, default=248)
    parser.add_argument("--seconds", type=float, default=0, help="0 = sans fin")
    args = parser.parse_args(argv)

    from netiface import join, udp_socket
    event, ip = udp_socket(args.iface, ttl=1, tos=0xB8, bind_port=EVENT_PORT)
    general, _ = udp_socket(args.iface, ip=ip, ttl=1, tos=0xB8, bind_port=GENERAL_PORT)
    join(event, PTP_GROUP, ip)
    join(general, PTP_GROUP, ip)
    clock_id = clock_identity(interface_mac(args.iface))
    print(f"grandmaster {clock_id.hex('-').upper()} domaine {args.domain} sur {args.iface} ({ip})")

    sync_period, announce_period = 2.0 ** args.sync_log, 2.0 ** args.announce_log
    seq_sync = seq_ann = 0
    next_sync = next_ann = time.monotonic()
    end = time.monotonic() + args.seconds if args.seconds else None
    while end is None or time.monotonic() < end:
        now = time.monotonic()
        if now >= next_ann:
            general.sendto(announce(args.domain, clock_id, seq_ann, args.announce_log, priority1=args.priority1),
                           (PTP_GROUP, GENERAL_PORT))
            seq_ann, next_ann = seq_ann + 1, next_ann + announce_period
        if now >= next_sync:
            t_send = now_tai_ns()
            event.sendto(sync(args.domain, clock_id, seq_sync, args.sync_log), (PTP_GROUP, EVENT_PORT))
            general.sendto(follow_up(args.domain, clock_id, seq_sync, args.sync_log, t_send), (PTP_GROUP, GENERAL_PORT))
            seq_sync, next_sync = seq_sync + 1, next_sync + sync_period
        timeout = max(0.0, min(next_sync, next_ann) - time.monotonic())
        readable, _, _ = select.select([event], [], [], timeout)
        if readable:
            data, _ = event.recvfrom(1500)
            t_recv = now_tai_ns()
            if len(data) >= 44 and data[0] & 0x0F == DELAY_REQ and data[4] == args.domain:
                general.sendto(delay_resp(args.domain, clock_id, data, t_recv, 0x7F), (PTP_GROUP, GENERAL_PORT))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except PermissionError:
        sys.exit("ports 319/320 : relancer avec sudo")
    except KeyboardInterrupt:
        sys.exit(0)
