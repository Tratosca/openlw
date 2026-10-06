# tools/lw — Python Livewire tools

Python 3 (standard library only; `pytest` for tests). Commands assume the repository root as the working directory. Specification: [docs/protocol/](../../docs/protocol/README.md).

| Tool | Purpose |
|---|---|
| `lwchan.py` | Channel ↔ multicast group (stereo, backfeed, surround) |
| `advcodec.py` | Envelope + TlvMsg codec, advertisement construction and decoding |
| `pcapio.py` | pcap/pcapng reading, Ethernet/802.1Q/IPv4/UDP decoding, synthetic pcap writing |
| `rtpstats.py` | Per-stream RTP statistics (PT, SSRC, size, Δseq, Δts, jitter, TOS, TTL) |
| `lwdump.py` | Capture summary: ADV, Livewire clock (7000), PTP (319/320) |
| `sdptool.py` | AES67 SDP generation and validation |
| `netiface.py` | UDP sockets bound to an interface (`IP_BOUND_IF` / `SO_BINDTODEVICE`, `IP_MULTICAST_IF`) |
| `mcast_join.py` | Keep groups joined during a capture (IGMP snooping) |
| `emit_rtp.py` | Test RTP transmitter (standard, aes67, livestream, surround) |
| `emit_adv.py` | Mock source advertisement |
| `ptp_gm.py` | Software PTPv2 test grandmaster (Sync/Follow_Up/Announce, Delay_Resp); no BMCA |
| `make_vectors.py` | Regenerate `docs/protocol/vectors/` |

Tests:

```sh
python3 -m pytest -q tools/lw/tests
```

Transmitters (`emit_*`) send real multicast traffic on the specified interface. Use them only on a test network, never on an operational Livewire network (a duplicate channel interrupts the legitimate source).
