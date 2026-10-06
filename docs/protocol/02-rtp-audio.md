# 02 — RTP audio streams

## Formats

Interleaved linear PCM audio, 48 kHz. Big-endian L24 (RFC 3190) by default; L16 can be advertised (`FAST = 3`, see [03](03-advertisement.md)).

| Format | Samples per packet | Interval | Packets/s | Stereo L24 payload | Evidence |
|---|---|---|---|---|---|
| Standard | 240 | 5 ms | 200 | 1,440 bytes | Observed |
| Livestream | 12 | 0.25 ms | 4,000 | 72 bytes | Observed (size), OpenLW choice (transmission) |
| AES67 | 48 | 1 ms | 1,000 | 288 bytes | AES67 |
| 8-channel surround | 60 | 1.25 ms | 800 | 1,440 bytes | Hypothesis |

Payloads remain at or below 1,440 bytes: surround keeps the Standard packet size.

## RTP header

12-byte header, no CSRC or extension (RFC 3550).

| Field | Value | Evidence |
|---|---|---|
| V, P, X, CC | 2, 0, 0, 0 (byte `0x80`) | Observed |
| M | 0 | Observed |
| PT | 96 by default (dynamic) | Observed |
| Sequence | +1 per packet | Observed |
| Timestamp | +samples per packet (Δ = 240 for Standard) | Observed (Standard) |
| SSRC | The four bytes of the destination group address | OpenLW choice |

At stream startup, OpenLW derives the sequence and timestamp from the Mac's clock (48 kHz samples since system startup). A restarted stream therefore moves forward rather than restarting at zero; a receiver sees loss, not a backward jump. OpenLW choice.

Reception: OpenLW assumes a 12-byte header and derives the sample count from the UDP length. A sequence difference in [−399, 0] is treated as a duplicate or late packet; a larger forward difference counts as loss; any other difference triggers resynchronization.

## IP, UDP, QoS

| Field | Value | Evidence |
|---|---|---|
| Multicast TTL | 128 | Observed |
| DSCP | EF (46, TOS byte `0xB8`) by default; AES67 recommends AF41 (34); configurable | Observed (EF) |
| Source port | Same as destination port (5004) | Observed |
| UDP checksum | Calculated by the Mac's network stack | OpenLW choice |

Sockets are bound to the Livewire interface (`IP_BOUND_IF`, `IP_MULTICAST_IF`). Group membership uses IGMP (macOS stack).

## SDP (AES67)

OpenLW generates and reads SDP descriptions conforming to RFC 4566 and RFC 7273. Generated lines use CRLF endings:

```
v=0
o=- <sess-id> <sess-version> IN IP4 <host-ip>
s=<source-name>
c=IN IP4 <group>
t=0 0
m=audio <port> RTP/AVP <pt>
a=rtpmap:<pt> L<bits>/<rate>/<channels>
a=sendonly
a=ptime:<duration>
[a=ts-refclk:ptp=IEEE1588-2008:<grandmaster-identity>:<domain>
 a=mediaclk:direct=0]
```

- `a=ptime`: integer milliseconds if the packet duration is integral (`1`, `5`), otherwise two decimal places (`1.25`, `0.25`).
- `ts-refclk` and `mediaclk` appear only when a PTP grandmaster is known.
- When parsing, `a=sync-time:<n>` (Ravenna variant) is accepted as `a=mediaclk:direct=<n>`.

Vector: [vectors/sdp/aes67-ch101.sdp](vectors/sdp/aes67-ch101.sdp). Media clock: [04-clock.md](04-clock.md).

Header vectors: [vectors/rtp_headers.json](vectors/rtp_headers.json).
