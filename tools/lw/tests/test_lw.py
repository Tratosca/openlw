"""Tests des outils de RE (fixtures synthetiques uniquement, aucune capture reelle)."""
import json
import pathlib
import struct
import sys

import pytest

HERE = pathlib.Path(__file__).resolve()
sys.path.insert(0, str(HERE.parents[1]))
VECTORS = HERE.parents[3] / "docs" / "protocol" / "vectors"

import advcodec  # noqa: E402
import lwdump  # noqa: E402
import lwchan  # noqa: E402
import pcapio  # noqa: E402
import rtpstats  # noqa: E402
import sdptool  # noqa: E402


@pytest.mark.parametrize("channel,kind,group", [
    (1, "stereo", "239.192.0.1"), (101, "stereo", "239.192.0.101"), (257, "stereo", "239.192.1.1"),
    (32766, "stereo", "239.192.127.254"), (5, "surround", "239.196.0.5"), (101, "backfeed", "239.193.0.101"),
])
def test_channel_mapping(channel, kind, group):
    assert lwchan.channel_to_group(channel, kind) == group
    assert lwchan.group_to_channel(group) == (channel, kind)


@pytest.mark.parametrize("channel", [0, 0x7FFF, 0x8000, -1])
def test_invalid_channels(channel):
    with pytest.raises(ValueError):
        lwchan.channel_to_group(channel)


def test_not_livewire_group():
    assert lwchan.group_to_channel("239.192.255.2") is None  # 0x7FFF + bit haut -> horloge, pas un canal
    assert lwchan.group_to_channel("224.0.1.129") is None


def test_vectors_chan2mcast():
    data = json.loads((VECTORS / "chan2mcast.json").read_text())
    for row in data["valid"]:
        assert lwchan.channel_to_group(row["channel"], row["kind"]) == row["group"]


def test_tlv_roundtrip_all_types():
    inner = advcodec.TlvMsg("INDI").add("PSID", advcodec.T_DWORD, 101)
    msg = (advcodec.TlvMsg("TEST")
           .add("DWRD", advcodec.T_DWORD, 0xDEADBEEF).add("BYTE", advcodec.T_BYTE, 7)
           .add("WORD", advcodec.T_WORD, 4000).add("QWRD", advcodec.T_QWORD, 0x0102030405060708)
           .add("BYTS", advcodec.T_BYTES, b"abc\0").add("STRG", advcodec.T_STRING, b"hello")
           .add("WRDS", advcodec.T_WORDS, [1, 2, 3]).add("DWDS", advcodec.T_DWORDS, [4, 5])
           .add("NEST", advcodec.T_MSG, inner))
    raw = msg.encode()
    back = advcodec.TlvMsg.decode(raw)
    assert back.encode() == raw
    assert back.get("NEST").get("PSID") == 101
    assert back.get("QWRD") == 0x0102030405060708


def test_tlv_wire_layout():
    raw = advcodec.TlvMsg("NEST").add("PVER", advcodec.T_WORD, 2).encode()
    assert raw == b"NEST" + b"\x00\x01" + b"PVER" + b"\x08" + b"\x00\x02"


def test_envelope_header_layout():
    msg = advcodec.TlvMsg("NEST")
    dgram = advcodec.encode_datagram(msg, seq=0x01020304)
    assert dgram[:4] == bytes([3, 0, 2, 7])
    assert dgram[4:8] == b"\x01\x02\x03\x04"
    assert len(dgram) == 16 + 6


def test_tlv_truncated_is_rejected():
    raw = advcodec.encode_datagram(advcodec.advertisement(1, "10.0.0.1", [(1, advcodec.source_block(1, "X"))], name="t"), 1)
    for cut in (10, 20, len(raw) - 1):
        with pytest.raises(advcodec.DecodeError):
            advcodec.decode_datagram(raw[:cut])


def test_vectors_adv():
    data = json.loads((VECTORS / "adv_packets.json").read_text())
    for pkt in data["packets"]:
        hdr, msg = advcodec.decode_datagram(bytes.fromhex(pkt["hex"]))
        assert hdr["envelope_version"] == 7 and hdr["seq"] == pkt["seq"]
        assert msg.to_dict() == pkt["expect"]


def test_advertisement_fields():
    src = advcodec.source_block(4001, "MAC TEST")
    msg = advcodec.advertisement(3, "192.168.10.20", [(1, src)], name="openlw-test")
    summary = advcodec.summarize(advcodec.TlvMsg.decode(msg.encode()))
    s = summary["sources"]["S001"]
    assert s["PSID"] == 4001 and s["LPID"] == 4001 and s["FAST"] == 2
    assert s["FSID"] == "239.192.15.161" and s["BSID"] == "239.193.15.161"
    assert summary["terminal"]["INIP"] == "192.168.10.20" and summary["terminal"]["HWID"] == (10 << 8) | 20
    with pytest.raises(ValueError):
        advcodec.advertisement(1, "10.0.0.1", [(i, src) for i in range(1, 10)])


def test_pcap_roundtrip_and_rtpstats(tmp_path):
    group = lwchan.channel_to_group(101)
    ssrc = struct.unpack(">I", bytes(int(x) for x in group.split(".")))[0]
    records = []
    for k in range(50):
        payload = struct.pack(">BBHII", 0x80, 96, k, k * 240, ssrc) + bytes(1440)
        records.append((1000 + k * 0.005, pcapio.build_udp_frame("192.168.10.5", group, 5004, 5004, payload)))
    path = tmp_path / "s.pcap"
    pcapio.write_pcap(path, records)
    (report,) = rtpstats.analyze(path, {5004})
    assert report["packets"] == 50 and report["livewire_channel"] == 101
    assert report["ssrc_equals_dst"] is True
    assert report["dts"] == {240: 49} and report["dseq"] == {1: 49}
    assert report["samples_per_packet_L24"] == 240
    assert report["pkt_rate"] == pytest.approx(200, rel=1e-3)
    assert report["ttl"] == {128: 50} and report["udp_checksum_zero"] == 50


def test_sdp_template():
    sdp = sdptool.generate("239.192.0.101", "192.168.10.20", "MAC", samples=48, gm="00-1d-c1-ff-fe-12-34-56", domain=0)
    lines = sdp.split("\r\n")
    assert lines[:9] == ["v=0", "o=- 0 0 IN IP4 192.168.10.20", "s=MAC", "c=IN IP4 239.192.0.101", "t=0 0",
                         "m=audio 5004 RTP/AVP 96", "a=rtpmap:96 L24/48000/2", "a=sendonly", "a=ptime:1"]
    assert "a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0" in lines
    assert "a=mediaclk:direct=0" in lines
    info, problems = sdptool.check(sdp)
    assert info["group"] == "239.192.0.101" and not problems


@pytest.mark.parametrize("samples,text", [(48, "1"), (240, "5"), (60, "1.25"), (12, "0.25"), (100, "1")])
def test_ptime_rule(samples, text):
    assert sdptool.ptime_text(samples) == text


def test_clock_vectors_decode():
    data = json.loads((VECTORS / "lwclock.json").read_text())
    prev = None
    for pkt in data["packets"]:
        raw = bytes.fromhex(pkt["hex"])
        assert len(raw) == data["expected_len"]
        c = lwdump.clock_packet(raw)
        assert c["rtp"] and c["x"] and c["ext_ok"]
        assert c["clock_type"] == "A" and c["clock_seq"] == pkt["clock_seq"] and c["ts"] == pkt["rtp_ts"]
        assert c["master_id"] == "c0a80a14"
        if prev:
            assert c["ts"] - prev["ts"] == data["ts_step"] and c["clock_seq"] - prev["clock_seq"] == 1
        prev = c


def test_ptp_gm_messages_match_windows_driver_offsets():
    import ptp_gm
    cid = ptp_gm.clock_identity(bytes.fromhex("001dc1123456"))
    assert cid.hex() == "001dc1fffe123456"
    t = 1_760_000_000 * 1_000_000_000 + 123_456_789
    s, f, a = ptp_gm.sync(0, cid, 7, -3), ptp_gm.follow_up(0, cid, 7, -3, t), ptp_gm.announce(5, cid, 1, 0)
    assert (len(s), len(f), len(a)) == (44, 44, 64)
    # PTP : octet 0 & 0xF = type, octet 4 = domaine, octet 6 bit 1 = two-step
    assert s[0] & 0xF == 0 and s[6] & 0x02 and f[0] & 0xF == 8 and a[0] & 0xF == 0xB and a[4] == 5
    # association Sync/Follow_Up : clockIdentity (20-27), port (28-29), sequenceId (30-31)
    assert s[20:32] == f[20:32]
    fu = lwdump.ptp_message(f)
    assert fu["timestamp"] == "1760000000.123456789" and fu["seq"] == 7
    assert fu["media_clock_48k"] == (1_760_000_000 * 48000 + 123_456_789 * 48000 // 10**9) & 0xFFFFFFFF
    an = lwdump.ptp_message(a)
    assert an["gm_identity"] == cid.hex("-") and an["gm_priority1"] == 248 and an["domain"] == 5
    assert ptp_gm.parse_timestamp(f[34:44]) == t


def test_ptp_gm_delay_resp():
    import ptp_gm
    cid = ptp_gm.clock_identity(b"\x02\x00\x00\x00\x00\x01")
    req = ptp_gm.header(ptp_gm.DELAY_REQ, 0, b"\x11" * 8, 42, 0x7F) + ptp_gm.timestamp(0)
    resp = ptp_gm.delay_resp(0, cid, req, 5_000_000_000, 0x7F)
    assert len(resp) == 54 and resp[0] & 0xF == 9 and resp[30:32] == b"\x00\x2a"
    assert resp[44:54] == req[20:30] and ptp_gm.parse_timestamp(resp[34:44]) == 5_000_000_000
