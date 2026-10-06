"""Read pcap / pcapng (standard library), decode Ethernet / 802.1Q / IPv4 / UDP.

    for pkt in udp_packets("capture.pcapng"):
        pkt.ts, pkt.src, pkt.dst, pkt.sport, pkt.dport, pkt.payload, pkt.tos, pkt.ttl, pkt.vlan
"""
import ipaddress
import struct
from dataclasses import dataclass

LINKTYPE_ETHERNET = 1


@dataclass
class UdpPacket:
    ts: float
    src: str
    dst: str
    sport: int
    dport: int
    payload: bytes
    tos: int
    ttl: int
    ip_id: int
    udp_checksum: int
    vlan: int = None
    pcp: int = None


def _read_pcap(f, magic):
    endian = "<" if magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1") else ">"
    nano = magic in (b"\x4d\x3c\xb2\xa1", b"\xa1\xb2\x3c\x4d")
    header = f.read(20)
    linktype = struct.unpack(endian + "HHiIII", header)[5]
    div = 1e9 if nano else 1e6
    while True:
        rec = f.read(16)
        if len(rec) < 16:
            return
        sec, frac, incl, _ = struct.unpack(endian + "IIII", rec)
        data = f.read(incl)
        if len(data) < incl:
            return
        yield sec + frac / div, linktype, data


def _read_pcapng(f):
    endian = "<"
    interfaces = []
    first = True
    while True:
        head = f.read(8)
        if len(head) < 8:
            return
        if first:
            # Section Header Block carries byte-order magic at offset 8
            bom = f.read(4)
            endian = "<" if bom == b"\x4d\x3c\x2b\x1a" else ">"
            btype, blen = struct.unpack(endian + "II", head)
            body = f.read(blen - 12)
            first = False
            continue
        btype, blen = struct.unpack(endian + "II", head)
        body = f.read(blen - 8)
        if btype == 0x0A0D0D0A:  # New section
            endian = "<" if body[:4] == b"\x4d\x3c\x2b\x1a" else ">"
            interfaces = []
        elif btype == 1:  # Interface Description Block
            linktype = struct.unpack_from(endian + "H", body, 0)[0]
            resol = 1e6
            opts = body[8:-4]
            pos = 0
            while pos + 4 <= len(opts):
                code, length = struct.unpack_from(endian + "HH", opts, pos)
                if code == 0:
                    break
                if code == 9 and length >= 1:  # if_tsresol
                    v = opts[pos + 4]
                    resol = 2 ** (v & 0x7F) if v & 0x80 else 10 ** v
                pos += 4 + ((length + 3) & ~3)
            interfaces.append((linktype, resol))
        elif btype == 6:  # Enhanced Packet Block
            iface, tsh, tsl, cap, _ = struct.unpack_from(endian + "IIIII", body, 0)
            linktype, resol = interfaces[iface] if iface < len(interfaces) else (LINKTYPE_ETHERNET, 1e6)
            yield ((tsh << 32) | tsl) / resol, linktype, body[20:20 + cap]
        elif btype == 3:  # Simple Packet Block
            linktype, _ = interfaces[0] if interfaces else (LINKTYPE_ETHERNET, 1e6)
            yield 0.0, linktype, body[4:]


def frames(path):
    """Iterate (timestamp, linktype, frame) over pcap or pcapng."""
    with open(path, "rb") as f:
        magic = f.read(4)
        if magic == b"\x0a\x0d\x0d\x0a":
            f.seek(0)
            yield from _read_pcapng(f)
        elif magic in (b"\xd4\xc3\xb2\xa1", b"\xa1\xb2\xc3\xd4", b"\x4d\x3c\xb2\xa1", b"\xa1\xb2\x3c\x4d"):
            yield from _read_pcap(f, magic)
        else:
            raise ValueError(f"format de capture inconnu : {path}")


def parse_udp(ts, linktype, frame):
    """Return a UdpPacket or None (not IPv4/UDP, fragment, truncated frame)."""
    if linktype != LINKTYPE_ETHERNET or len(frame) < 14:
        return None
    pos = 12
    ethertype = struct.unpack_from(">H", frame, pos)[0]
    vlan = pcp = None
    if ethertype == 0x8100:
        tci = struct.unpack_from(">H", frame, 14)[0]
        vlan, pcp = tci & 0x0FFF, tci >> 13
        pos = 16
        ethertype = struct.unpack_from(">H", frame, pos)[0]
    if ethertype != 0x0800:
        return None
    ip = pos + 2
    if len(frame) < ip + 20:
        return None
    vihl, tos, total, ip_id, frag, ttl, proto = struct.unpack_from(">BBHHHBB", frame, ip)
    ihl = (vihl & 0x0F) * 4
    if vihl >> 4 != 4 or proto != 17 or frag & 0x1FFF or frag & 0x2000:
        return None
    udp = ip + ihl
    if len(frame) < udp + 8:
        return None
    sport, dport, ulen, csum = struct.unpack_from(">HHHH", frame, udp)
    payload = frame[udp + 8:udp + max(ulen, 8)]
    src = str(ipaddress.IPv4Address(frame[ip + 12:ip + 16]))
    dst = str(ipaddress.IPv4Address(frame[ip + 16:ip + 20]))
    return UdpPacket(ts, src, dst, sport, dport, payload, tos, ttl, ip_id, csum, vlan, pcp)


def udp_packets(path):
    for ts, linktype, frame in frames(path):
        pkt = parse_udp(ts, linktype, frame)
        if pkt is not None:
            yield pkt


def write_pcap(path, records):
    """Write a classic Ethernet pcap. records: iterable of (timestamp, frame)."""
    with open(path, "wb") as f:
        f.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, LINKTYPE_ETHERNET))
        for ts, frame in records:
            sec = int(ts)
            f.write(struct.pack("<IIII", sec, int((ts - sec) * 1e6), len(frame), len(frame)))
            f.write(frame)


def build_udp_frame(src, dst, sport, dport, payload, tos=0xB8, ttl=128, ip_id=0xD0F8, vlan=None):
    """Ethernet/IPv4/UDP frame (UDP checksum 0). For tests and vectors."""
    dst_ip = ipaddress.IPv4Address(dst)
    if dst_ip.is_multicast:
        b = dst_ip.packed
        dmac = bytes([0x01, 0x00, 0x5E, b[1] & 0x7F, b[2], b[3]])
    else:
        dmac = b"\x02\x00\x00\x00\x00\x02"
    smac = b"\x02\x00\x00\x00\x00\x01"
    eth = dmac + smac
    if vlan is not None:
        eth += struct.pack(">HH", 0x8100, vlan)
    eth += b"\x08\x00"
    udp = struct.pack(">HHHH", sport, dport, 8 + len(payload), 0) + payload
    ip = bytearray(struct.pack(">BBHHHBBH4s4s", 0x45, tos, 20 + len(udp), ip_id, 0, ttl, 17, 0,
                               ipaddress.IPv4Address(src).packed, dst_ip.packed))
    s = sum(struct.unpack(">10H", bytes(ip)))
    s = (s & 0xFFFF) + (s >> 16)
    s = (s & 0xFFFF) + (s >> 16)
    struct.pack_into(">H", ip, 10, ~s & 0xFFFF)
    return eth + bytes(ip) + udp
