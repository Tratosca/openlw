#!/usr/bin/env python3
"""Write eight synthetic Livewire frames to an Ethernet pcap, without dependencies."""
import argparse
import ipaddress
import struct
from pathlib import Path


def item(tag, kind, value):
    """Serialize a TlvMsg item in network order."""
    formats = {1: "!I", 7: "!B", 8: "!H", 9: "!Q"}
    if kind in formats:
        payload = struct.pack(formats[kind], value)
    elif kind in (2, 3, 6):
        payload = struct.pack("!H", len(value)) + value
    elif kind in (4, 5):
        payload = struct.pack("!H", len(value)) + struct.pack(
            "!" + ("H" if kind == 4 else "I") * len(value), *value
        )
    else:
        raise ValueError(f"Unknown TlvMsg type: {kind}")
    return tag.encode("ascii") + bytes([kind]) + payload


def message(name, *items):
    return name.encode("ascii") + struct.pack("!H", len(items)) + b"".join(items)


def advertisement(short=False, seq=1):
    term = [item("ADVV", 1, 1), item("HWID", 8, 10),
            item("INIP", 1, 0xC0A8010A), item("UDPC", 8, 4000), item("NUMS", 8, 1)]
    if not short:
        term.append(item("ATRN", 2, b"LW-SAMPLE".ljust(32, b"\0")))
    parts = [item("PVER", 8, 2), item("ADVT", 7, 2 if short else 1),
             item("TERM", 6, message("INDI", *term))]
    if not short:
        source = [item("PSID", 1, 101), item("SHAB", 7, 1),
                  item("FSID", 1, 0xEFC00065), item("FAST", 7, 2), item("FASM", 7, 1),
                  item("BSID", 1, 0xEFC10065), item("BAST", 7, 2), item("BASM", 7, 1),
                  item("LPID", 1, 101), item("STPL", 7, 0),
                  item("PSNM", 2, b"Source 101".ljust(16, b"\0"))]
        parts.append(item("S001", 6, message("INDI", *source)))
    return struct.pack("!BBBBIHHI", 3, 0, 2, 7, seq, 4000, 0, 0) + message("NEST", *parts)


def rtp(seq, timestamp, ssrc, payload):
    return struct.pack("!BBHII", 0x80, 96, seq, timestamp, ssrc) + payload


def checksum(data):
    """IPv4 checksum (one's complement)."""
    words = struct.unpack("!" + "H" * (len(data) // 2), data)
    total = sum(words)
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def ethernet(payload, destination, port, ident):
    dst = ipaddress.IPv4Address(destination).packed
    src = ipaddress.IPv4Address("192.168.1.10").packed
    udp = struct.pack("!HHHH", port, port, 8 + len(payload), 0) + payload
    header = struct.pack("!BBHHHBBH4s4s", 0x45, 0xB8, 20 + len(udp), ident,
                         0, 128, 17, 0, src, dst)
    header = header[:10] + struct.pack("!H", checksum(header)) + header[12:]
    mac = bytes([1, 0, 0x5E, dst[1] & 0x7F, dst[2], dst[3]])
    return mac + bytes.fromhex("02000000000a0800") + header + udp


def packets():
    yield advertisement(), "239.192.255.3", 4001
    yield advertisement(short=True, seq=2), "239.192.255.3", 4001
    # Interleaved L24 silence; three consecutive packets with 240 samples/channel.
    for n in range(3):
        yield rtp(100 + n, 48000 + 240 * n, 0xEFC00065, bytes(240 * 2 * 3)), "239.192.0.101", 5004
    yield rtp(200, 48000, 0xEFC40005, bytes(60 * 8 * 3)), "239.196.0.5", 5004
    # 44-byte total UDP payload: 12 RTP + 32 hypothetical bytes.
    clock = bytearray(rtp(300, 48000, 0xEFC0FF02, bytes(32)))
    clock[26:30] = bytes.fromhex("12345678")
    yield bytes(clock), "239.192.255.2", 7000
    # Consistent Ethernet/IP/UDP lengths, but final TlvMsg incomplete.
    yield advertisement(seq=3)[:-5], "239.192.255.3", 4001


def write_pcap(path):
    with Path(path).open("wb") as output:
        output.write(struct.pack("<IHHIIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 1))
        for index, (payload, destination, port) in enumerate(packets(), 1):
            frame = ethernet(payload, destination, port, index)
            output.write(struct.pack("<IIII", 1700000000, (index - 1) * 5000, len(frame), len(frame)))
            output.write(frame)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="Output pcap path (outside the repository)")
    args = parser.parse_args()
    write_pcap(args.output)
    print(f"8 packets written: {args.output}")


if __name__ == "__main__":
    main()
