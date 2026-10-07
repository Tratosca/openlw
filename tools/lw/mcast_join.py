#!/usr/bin/env python3
"""Join multicast groups on an interface and maintain membership while running.

Used during captures when IGMP snooping hides streams (docs/protocol/02-rtp-audio.md).

    python3 mcast_join.py --iface en7 --groups 239.192.255.2 239.192.255.3 224.0.1.129 --channels 1 2 101
"""
import argparse
import signal
import socket
import sys
import time

from lwchan import channel_to_group
from netiface import join, udp_socket

DEFAULT_GROUPS = ["239.192.255.2", "239.192.255.3", "224.0.1.129"]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--iface", required=True, help="BSD interface name (enX)")
    parser.add_argument("--groups", nargs="*", default=DEFAULT_GROUPS)
    parser.add_argument("--channels", nargs="*", type=int, default=[], help="Livewire stereo channels to join")
    parser.add_argument("--surround", nargs="*", type=int, default=[], help="surround channels to join")
    args = parser.parse_args(argv)
    groups = list(args.groups) + [channel_to_group(c) for c in args.channels] + [channel_to_group(c, "surround") for c in args.surround]
    sock, ip = udp_socket(args.iface)
    for g in groups:
        join(sock, g, ip)
        print(f"join {g} on {args.iface} ({ip})")
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    print("Ctrl-C to quit (groups are left when the socket closes)")
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        sock.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
