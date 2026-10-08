# 04 — Clock

## OpenLW behavior

OpenLW does not yet synchronize to a network clock: the Mac's clock paces audio (nominal 48 kHz). Audio exchange works with the tested devices without a common clock. Reception buffers compensate for clock differences through slips: occasional very brief interruptions when accumulated drift exceeds the buffer. Synchronization is planned (PTP, then Livewire clock).

## Livewire clock

| Item | Value | Evidence |
|---|---|---|
| Transport | Multicast **239.192.255.2**, UDP **7000** | Observed |
| Structure | RTP packet with header extension, profile `0xFA1A`, length 20 words (80 bytes) | Hypothesis |
| RTP timestamp | 48 kHz sample counter | Hypothesis |
| Timing | One packet every 250 µs (timestamp +12) | Hypothesis |

Assumed UDP payload content (to be confirmed by capture):

| Bytes | Contents |
|---|---|
| 0–11 | RTP header |
| 12–13 | Extension profile `FA 1A` |
| 14–15 | Extension length `00 14` |
| 16–19 | Clock sequence number |
| 20–23 | Message type: `0A 00 CA BA` (A) or `0B 00 CA BA` (B) |
| 26–29 | Master identifier |
| Other | Unknown |

OpenLW decodes these packets (`daemon/lw-proto/src/lwclock.rs`, `tools/lw/lwdump.py`) but does not send them. Vectors: [vectors/lwclock.json](vectors/lwclock.json).

## PTP (AES67)

PTPv2 (IEEE 1588-2008), AES67 media profile: group 224.0.1.129, ports 319 (event) and 320 (general), configurable domain (default 0). AES67 stream media clock (RFC 7273, `mediaclk:direct=0`):

```
RTP timestamp = seconds × 48,000 + nanoseconds × 48,000 / 10⁹   (modulo 2³²)
```

OpenLW contains a PTP decoder (`daemon/lw-proto/src/ptp.rs`) and a software test grandmaster (`tools/lw/ptp_gm.py`), but no slave yet.
