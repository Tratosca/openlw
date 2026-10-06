# 01 — Livewire channels and multicast groups

A Livewire channel is an integer **N from 1 to 32,766**. The channel occupies the low 16 bits of the stream's multicast address; the prefix depends on the stream type.

| Stream type | Group | Evidence |
|---|---|---|
| Stereo (Standard, Livestream, AES67) | `239.192.(N >> 8).(N & 0xFF)` | Observed |
| Return to source (backfeed) | `239.193.(N >> 8).(N & 0xFF)` | Observed in advertisements (`BSID`) |
| 8-channel surround | `239.196.(N >> 8).(N & 0xFF)` | Hypothesis |

Inverse: `N = address & 0x7FFF`.

Examples: channel 1 → `239.192.0.1`; channel 101 → `239.192.0.101`; channel 257 → `239.192.1.1`; channel 32,766 → `239.192.127.254`.

Audio port: **UDP 5004** (observed).

Vectors: [vectors/chan2mcast.json](vectors/chan2mcast.json). Implementations: `daemon/lw-proto/src/channel.rs`, `tools/lw/lwchan.py`.
