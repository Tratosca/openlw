# OpenLW network specification

This directory describes what OpenLW sends and accepts on a Livewire® / AES67 network: channel addressing, RTP audio streams, source advertisements, and clocks. It is the reference for tests (`vectors/`) and tools (`tools/lw/`, `tools/wireshark/`).

Information comes from traffic observed on compatible devices (network captures) and public standards: RFC 3550 (RTP), RFC 3190 (L24), RFC 4566 (SDP), RFC 7273 (media clocks), AES67, and IEEE 1588 (PTP).

| Document | Subject |
|---|---|
| [01-channels.md](01-channels.md) | Livewire channels and multicast groups |
| [02-rtp-audio.md](02-rtp-audio.md) | Audio streams: formats, RTP headers, SDP, QoS |
| [03-advertisement.md](03-advertisement.md) | Source advertisements and discovery |
| [04-clock.md](04-clock.md) | Livewire clock and PTP |
| [open-questions.md](open-questions.md) | Unverified points |

## Conventions

Where needed, statements carry one of these labels:

- **Observed:** seen in captures of Livewire device traffic.
- **OpenLW choice:** OpenLW behavior compatible with observed devices, but not mandated by them.
- **Hypothesis:** assumed, not verified on the network; listed in [open-questions.md](open-questions.md).

Bytes are in network order (big-endian) unless stated otherwise.

## Trademarks

Livewire, Livewire+, and Axia are trademarks of TLS Corp. (Telos Alliance). OpenLW is neither affiliated with nor endorsed by TLS Corp. These names identify the protocol with which OpenLW is compatible.
