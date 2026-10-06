#!/usr/bin/env python3
"""Envelope + TlvMsg codec and Livewire advertisement messages (docs/protocol/03-advertisement.md).

Command-line usage: decode a hexadecimal datagram.
    python3 advcodec.py 0300 0207 ...
"""
import ipaddress
import struct
import sys

# TlvMsg types
T_DWORD, T_BYTES, T_STRING, T_WORDS, T_DWORDS, T_MSG, T_BYTE, T_WORD, T_QWORD = 1, 2, 3, 4, 5, 6, 7, 8, 9

# Envelope header
ENVELOPE_HEADER_LEN = 16
ENVELOPE_VERSION = 7
TLV_VERSION = 2
LAYER_TLV = 3
MSG_DATAGRAM, MSG_MESSAGE, MSG_ACK, MSG_NACK = 0x00, ord("M"), ord("A"), ord("N")

MAX_DEPTH = 8


class DecodeError(ValueError):
    pass


def fourcc(tag):
    """'PSNM' -> 0x5053494E; integers are returned unchanged."""
    if isinstance(tag, int):
        return tag
    raw = tag.encode("ascii")
    if len(raw) != 4:
        raise ValueError(f"FourCC de 4 caracteres attendu : {tag!r}")
    return struct.unpack(">I", raw)[0]


def fourcc_str(value):
    raw = struct.pack(">I", value)
    return raw.decode("ascii") if all(0x20 <= b < 0x7F for b in raw) else f"0x{value:08X}"


class TlvMsg:
    """TLV message: FourCC identifier and ordered list of (tag, type, value) items."""

    def __init__(self, msg_id, items=None):
        self.msg_id = fourcc(msg_id)
        self.items = list(items or [])

    def add(self, tag, typ, value):
        self.items.append((fourcc(tag), typ, value))
        return self

    def get(self, tag, default=None):
        tag = fourcc(tag)
        for item_tag, _, value in self.items:
            if item_tag == tag:
                return value
        return default

    def to_dict(self):
        """Readable representation (for JSON and display)."""
        out = {"_id": fourcc_str(self.msg_id)}
        for tag, typ, value in self.items:
            key = fourcc_str(tag)
            if typ == T_MSG:
                out[key] = value.to_dict()
            elif typ in (T_BYTES, T_STRING):
                out[key] = value.split(b"\0", 1)[0].decode("latin-1")
            else:
                out[key] = value
        return out

    def encode(self):
        parts = [struct.pack(">IH", self.msg_id, len(self.items))]
        for tag, typ, value in self.items:
            parts.append(struct.pack(">IB", tag, typ))
            if typ == T_DWORD:
                parts.append(struct.pack(">I", value & 0xFFFFFFFF))
            elif typ == T_BYTE:
                parts.append(struct.pack(">B", value & 0xFF))
            elif typ == T_WORD:
                parts.append(struct.pack(">H", value & 0xFFFF))
            elif typ == T_QWORD:
                parts.append(struct.pack(">Q", value & 0xFFFFFFFFFFFFFFFF))
            elif typ in (T_BYTES, T_STRING):
                data = bytes(value)
                parts.append(struct.pack(">H", len(data)) + data)
            elif typ == T_WORDS:
                parts.append(struct.pack(f">H{len(value)}H", len(value), *value))
            elif typ == T_DWORDS:
                parts.append(struct.pack(f">H{len(value)}I", len(value), *value))
            elif typ == T_MSG:
                body = value.encode()
                parts.append(struct.pack(">H", len(body)) + body)
            else:
                raise ValueError(f"type TlvMsg inconnu : {typ}")
        data = b"".join(parts)
        if len(data) > 0xFFFF:
            raise ValueError("TlvMsg trop long")
        return data

    @classmethod
    def decode(cls, data, depth=0):
        msg, used = cls._decode(memoryview(bytes(data)), depth)
        return msg

    @classmethod
    def _decode(cls, buf, depth):
        if depth > MAX_DEPTH:
            raise DecodeError("imbrication trop profonde")
        if len(buf) < 6:
            raise DecodeError("en-tete TlvMsg tronque")
        msg_id, count = struct.unpack_from(">IH", buf, 0)
        msg = cls(msg_id)
        pos = 6
        for _ in range(count):
            if pos + 5 > len(buf):
                raise DecodeError("item tronque")
            tag, typ = struct.unpack_from(">IB", buf, pos)
            pos += 5

            def need(n):
                if pos + n > len(buf):
                    raise DecodeError(f"valeur tronquee (tag {fourcc_str(tag)})")

            if typ == T_DWORD:
                need(4); value = struct.unpack_from(">I", buf, pos)[0]; pos += 4
            elif typ == T_BYTE:
                need(1); value = buf[pos]; pos += 1
            elif typ == T_WORD:
                need(2); value = struct.unpack_from(">H", buf, pos)[0]; pos += 2
            elif typ == T_QWORD:
                need(8); value = struct.unpack_from(">Q", buf, pos)[0]; pos += 8
            elif typ in (T_BYTES, T_STRING, T_WORDS, T_DWORDS, T_MSG):
                need(2); n = struct.unpack_from(">H", buf, pos)[0]; pos += 2
                if typ in (T_BYTES, T_STRING):
                    need(n); value = bytes(buf[pos:pos + n]); pos += n
                elif typ == T_WORDS:
                    need(2 * n); value = list(struct.unpack_from(f">{n}H", buf, pos)); pos += 2 * n
                elif typ == T_DWORDS:
                    need(4 * n); value = list(struct.unpack_from(f">{n}I", buf, pos)); pos += 4 * n
                else:
                    need(n); value, _ = cls._decode(buf[pos:pos + n], depth + 1); pos += n
            else:
                raise DecodeError(f"type TlvMsg inconnu {typ} (tag {fourcc_str(tag)})")
            msg.items.append((tag, typ, value))
        return msg, pos


def envelope_header(seq, msg_type=MSG_DATAGRAM, layer=LAYER_TLV, result_port=0, lock_id=0, lock_tid=0):
    return struct.pack(">BBBBIHHI", layer, msg_type, TLV_VERSION, ENVELOPE_VERSION, seq, result_port, lock_id, lock_tid)


def encode_datagram(msg, seq, msg_type=MSG_DATAGRAM):
    return envelope_header(seq, msg_type) + msg.encode()


def decode_datagram(data):
    """Return (header dict, TlvMsg). Raise DecodeError unless datagram uses Envelope v7."""
    if len(data) < ENVELOPE_HEADER_LEN:
        raise DecodeError("datagramme plus court que l'en-tete Envelope")
    layer, msg_type, cmsg_ver, envelope_ver, seq, result_port, lock_id, lock_tid = struct.unpack_from(">BBBBIHHI", data, 0)
    if envelope_ver != ENVELOPE_VERSION:
        raise DecodeError(f"version Envelope {envelope_ver} (7 attendu)")
    header = dict(layer=layer, type=msg_type, tlv_version=cmsg_ver, envelope_version=envelope_ver, seq=seq,
                  result_port=result_port, lock_id=lock_id, lock_tid=lock_tid)
    return header, TlvMsg.decode(data[ENVELOPE_HEADER_LEN:])


def _ip(value):
    return int(ipaddress.IPv4Address(value)) if isinstance(value, str) else value


def _fixed(text, size):
    """Fixed-length ASCII string: transliterate accents (NFKD), replace other characters with '?'."""
    import unicodedata
    plain = unicodedata.normalize("NFKD", text).encode("ascii", "ignore").decode("ascii")
    plain = "".join(c if 32 <= ord(c) < 127 else "?" for c in plain)
    return plain.encode("ascii")[:size].ljust(size, b"\0")


def terminal_block(advv, ip, udpc=4000, nums=0, name=None, hwid=None):
    term = TlvMsg("INDI")
    ipv = _ip(ip)
    term.add("ADVV", T_DWORD, advv)
    term.add("HWID", T_WORD, (ipv & 0xFFFF) if hwid is None else hwid)
    term.add("INIP", T_DWORD, ipv)
    term.add("UDPC", T_WORD, udpc)
    term.add("NUMS", T_WORD, nums)
    if name is not None:
        term.add("ATRN", T_STRING, _fixed(name, 32))
    return term


def source_block(channel, name, fast=2, shareable=0, label=None, group=None):
    """Advertised source entry (FAST: 2 L24, 3 L16, 4 surround)."""
    if group is None:
        prefix = 0xEFC40000 if fast == 4 else 0xEFC00000
        group = prefix | channel
    src = TlvMsg("INDI")
    src.add("PSID", T_DWORD, channel)
    src.add("SHAB", T_BYTE, shareable)
    src.add("FSID", T_DWORD, _ip(group))
    src.add("FAST", T_BYTE, fast)
    src.add("FASM", T_BYTE, 1)
    src.add("BSID", T_DWORD, 0xEFC10000 | channel)
    src.add("BAST", T_BYTE, 0)
    src.add("BASM", T_BYTE, 1)
    src.add("LPID", T_DWORD, channel)
    src.add("STPL", T_BYTE, 0)
    src.add("PSNM", T_STRING, _fixed(name, 16))
    if label:
        src.add("LABL", T_STRING, _fixed(label, 10))
    return src


def advertisement(advv, ip, sources=(), name=None, full=True, udpc=4000):
    """Build an advertisement page (NEST). sources: list of (slot 1..240, source block), at most eight."""
    if len(sources) > 8:
        raise ValueError("au plus 8 sources par datagramme")
    msg = TlvMsg("NEST")
    msg.add("PVER", T_WORD, 2)
    msg.add("ADVT", T_BYTE, 1 if full else 2)
    msg.add("TERM", T_MSG, terminal_block(advv, ip, udpc, len(sources), name if full else None))
    if full:
        for slot, block in sources:
            msg.add(f"S{slot:03d}", T_MSG, block)
    return msg


def summarize(msg):
    """Advertisement summary: dictionary with terminal and sources."""
    term = msg.get("TERM")
    out = {"id": fourcc_str(msg.msg_id), "advt": msg.get("ADVT"), "pver": msg.get("PVER")}
    if isinstance(term, TlvMsg):
        out["terminal"] = term.to_dict()
        if "INIP" in out["terminal"]:
            out["terminal"]["INIP"] = str(ipaddress.IPv4Address(out["terminal"]["INIP"]))
    sources = {}
    for tag, typ, value in msg.items:
        name = fourcc_str(tag)
        if typ == T_MSG and name[0] == "S" and name[1:].isdigit():
            d = value.to_dict()
            for key in ("FSID", "BSID"):
                if key in d:
                    d[key] = str(ipaddress.IPv4Address(d[key]))
            sources[name] = d
    out["sources"] = sources
    return out


if __name__ == "__main__":
    import json
    payload = bytes.fromhex("".join(sys.argv[1:]) or sys.stdin.read().strip())
    hdr, message = decode_datagram(payload)
    print(json.dumps({"envelope": hdr, "message": summarize(message), "raw": message.to_dict()}, indent=2, ensure_ascii=False))
