#!/usr/bin/env python3
"""Livewire channel <-> multicast group conversion (docs/protocol/01-channels.md).

    python3 lwchan.py 101            -> 239.192.0.101 (stereo)
    python3 lwchan.py 5 --kind surround
    python3 lwchan.py 239.196.0.5    -> channel 5, surround
"""
import argparse
import ipaddress
import sys

CHANNEL_MIN = 1
CHANNEL_MAX = 0x7FFE  # 0x7FFF is reserved

PREFIXES = {
    "stereo": 0xEFC00000,    # Standard, AES67, Livestream
    "backfeed": 0xEFC10000,  # "To Source"
    "surround": 0xEFC40000,  # 8 channels
}
KINDS = {prefix: kind for kind, prefix in PREFIXES.items()}


def channel_to_group(channel, kind="stereo"):
    """Return a channel multicast group (str)."""
    if not CHANNEL_MIN <= channel <= CHANNEL_MAX:
        raise ValueError(f"channel out of range {CHANNEL_MIN}..{CHANNEL_MAX}: {channel}")
    if kind not in PREFIXES:
        raise ValueError(f"unknown kind: {kind}")
    return str(ipaddress.IPv4Address(PREFIXES[kind] | channel))


def group_to_channel(group):
    """Return (channel, kind) for a group, or None if not a Livewire audio group."""
    value = int(ipaddress.IPv4Address(group))
    kind = KINDS.get(value & 0xFFFF8000)
    channel = value & 0x7FFF
    if kind is None or not CHANNEL_MIN <= channel <= CHANNEL_MAX:
        return None
    return channel, kind


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("value", help="channel number or IPv4 address")
    parser.add_argument("--kind", choices=sorted(PREFIXES), default="stereo")
    args = parser.parse_args(argv)
    if "." in args.value:
        result = group_to_channel(args.value)
        if result is None:
            print(f"{args.value}: not a Livewire audio group", file=sys.stderr)
            return 1
        print(f"channel {result[0]} ({result[1]})")
    else:
        print(channel_to_group(int(args.value, 0), args.kind))
    return 0


if __name__ == "__main__":
    sys.exit(main())
