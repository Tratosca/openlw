# Livewire dissectors for Wireshark

`livewire.lua` decodes Envelope/TlvMsg (`lwadv`), clock (`lwclock`), and Livewire RTP (`lwrtp`), based on the [specification](../../docs/protocol/README.md).

## Installation

Prerequisites: Wireshark/TShark with Lua; Python 3 for the generator. Validated with TShark 4.6.2 and Lua 5.4.7 on macOS.

Copy `livewire.lua` into the **Personal Lua Plugins** directory shown in **About Wireshark → Folders**, then restart Wireshark. Alternatively, load the script explicitly from the repository root:

```sh
/Applications/Wireshark.app/Contents/MacOS/Wireshark \
  -X lua_script:tools/wireshark/livewire.lua

/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap -V
```

Do not combine a personal installation and `-X` loading of the same script. Calls use the [official Wireshark Lua APIs](https://www.wireshark.org/docs/wsdg_html_chunked/lua_module_Proto.html).

## Ports and preferences

In Livewire protocol preferences, or with `tshark -o name:value`:

| Preference | Default | Purpose |
|---|---:|---|
| `lwadv.control_port` | 4000 | Envelope request / control |
| `lwadv.announce_port` | 4001 | Envelope advertisement |
| `lwclock.port` | 7000 | RTP clock |
| `lwrtp.port` | 5004 | RTP audio |

Example: `-o lwrtp.port:15004`. Zero disables a binding; values above 65535 are ignored. Use distinct ports for each protocol. `lwrtp` is a dedicated UDP dissector that replaces normal RTP decoding on its port; it exposes its own fields, not `rtp.*` fields.

## Useful filters

```text
lwadv
lwadv.advt == 1
lwadv.message_id == "READ"
lwadv.tag == "PSNM"
lwadv.channel == 101
lwadv.ip == 239.192.0.101
lwclock
lwclock.master_id == 12:34:56:78
lwrtp.channel == 101
lwrtp.kind == "surround"
lwrtp.samples == 240
lwrtp.ssrc_matches_dst == false
_ws.malformed || _ws.expert.severity == error
```

FourCC tags are displayed as ASCII; non-printable bytes are escaped as `\xNN`. TlvMsg types 1–9 are decoded; arrays remain typed and u64 values retain precision. Types 2/3 display bytes and a string if the content is printable ASCII, after removing trailing NULs. PSID/LPID fields expose the channel masked to 15 bits; FSID/BSID/INIP also expose an IPv4 address.

The ADV summary shows the name and sources present in the packet without retaining earlier advertisements. A short advertisement therefore shows the dissector's current French placeholders `<absent>` for ATRN and `<aucune>` for sources. A malformed packet may have a partial summary; inspect expert information.

## Generation and validation

The generator uses only the Python standard library. It writes a classic Ethernet pcap without transmitting network traffic. The supplied path is overwritten. Keep captures outside the repository:

```sh
python3 tools/wireshark/make_sample_pcap.py /tmp/lw_sample.pcap
/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap -V
/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap \
  -Y "_ws.malformed || _ws.expert.severity == error"
```

Expected result: eight packets, only **packet 8** matches the expert filter, no Lua errors.

| Packet | Expected contents |
|---|---|
| 1 | Full ADV, LW-SAMPLE, NUMS=1, S001, channel 101 |
| 2 | Short ADV, NUMS=1, no ATRN or source |
| 3–5 | RTP PT 96, stereo, channel 101, 240 samples, sequences 100–102 |
| 6 | RTP PT 96, surround, channel 5, 60 samples on 8 channels |
| 7 | Clock: total UDP payload 44 bytes, identity `12345678` |
| 8 | ADV deliberately truncated inside the S001 submessage |

## Limitations

- The complete clock format remains a **hypothesis**. Offsets 26–29 are zero-based from the UDP payload start. The rest of the RTP payload is raw; the generator's PT, SSRC, and synthetic data do not establish a hardware format.
- Audio annotations assume **L24**: RTP payload size divided by `3 × channel count`, with eight channels for `239.196/16`, otherwise two for `239.192/16` and `239.193/16`. Dynamic PT does not establish encoding. L16 and SDP sessions are not identified automatically. CSRCs, extensions, and RTP padding are excluded from payload calculations.
- TlvMsg is limited to **eight levels including the root**. Truncation, sub-length overrun, unknown types, and excessive depth produce `Malformed/Error` expert information. Envelope ACK/NACK packets without TlvMsg are accepted.
- No multipage advertisement reconstruction or replacement of the native PTP dissector. Other messages received on an advertisement port remain readable as generic TlvMsg.
- Tests are synthetic and built from the specification; they do not validate interoperability with real Livewire equipment.
