# 03 — Source advertisements and discovery

Livewire devices advertise their sources through multicast. OpenLW listens to these advertisements to discover network sources and advertises the channels it transmits.

## Transport

| Item | Value | Evidence |
|---|---|---|
| Advertisements | Multicast **239.192.255.3**, UDP **4001** | Observed |
| Terminal control port | UDP **4000** (advertised in `UDPC`), unicast | Observed |
| TTL | 128 | Observed |

One datagram = **16-byte envelope** + **TLV message**.

## Envelope (16 bytes)

| Byte(s) | Observed value | Role |
|---|---|---|
| 0 | 3 | Message layer (TLV message) |
| 1 | 0 | Datagram without acknowledgment |
| 2 | 2 | TLV format version |
| 3 | 7 | Envelope version |
| 4–7 | Sequence number | Incremented on each send, never 0 |
| 8–15 | 0 | Unused in advertisements |

OpenLW rejects datagrams whose byte 3 is not 7.

## TLV message

```
u32  message identifier (four ASCII characters, e.g. 'NEST')
u16  entry count
entries:
  u32  tag (four ASCII characters, e.g. 'PSNM')
  u8   type
  ...  value according to type
```

| Type | Value |
|---|---|
| 1 | u32 |
| 2 | u16 length + bytes |
| 3 | u16 length + string (fixed length, zero-padded) |
| 4 | u16 count + count × u16 |
| 5 | u16 count + count × u32 |
| 6 | u16 length + nested TLV message (identifier followed by entries) |
| 7 | u8 |
| 8 | u16 |
| 9 | u64 |

An observed nested message has the identifier `INDI`, followed by the entry count like any message.

## Full and short advertisements

```
'NEST'
  'PVER'  u16  2
  'ADVT'  u8   1 = full advertisement, 2 = short advertisement (keepalive)
  'TERM'  message 'INDI': the terminal
     'ADVV'  u32  advertisement version (changes when the source list changes)
     'HWID'  u16  terminal identifier (low 16 bits of its IP address)
     'INIP'  u32  terminal IP address
     'UDPC'  u16  control port (4000)
     'NUMS'  u16  advertised source count
     'ATRN'  string[32]  terminal name (full advertisement only)
  'S001' … 'S240'  message 'INDI': one source per slot (full advertisement only)
     'PSID'  u32  Livewire channel
     'SHAB'  u8   shareable source
     'FSID'  u32  forward-stream group (239.192.x.y)
     'FAST'  u8   forward-stream type: 2 L24 stereo, 3 L16 stereo, 4 surround
     'FASM'  u8   forward-stream mode (1)
     'BSID'  u32  return group (239.193.x.y)
     'BAST'  u8   return-stream type
     'BASM'  u8   return-stream mode
     'LPID'  u32  channel (copy of PSID)
     'STPL'  u8   0
     'PSNM'  string[16]  source name
     'LABL'  string[10]  label (optional)
```

Evidence: structure, tags, and string types (3) observed; meanings of `SHAB`, `FASM`, `BAST`, `BASM`, and `STPL` are hypotheses.

- A full advertisement contains at most **8 sources per datagram**; larger lists are sent in successive pages with the same `ADVV`.
- A short advertisement uses the same `NEST` envelope, with `ADVT = 2`, a `TERM` without `ATRN`, and no sources.
- A source tag is `S` followed by three ASCII digits (slot 1–240).
- Names use ASCII: OpenLW replaces accented letters (“François” → “Francois”) and pads with zeros.

### Values chosen by OpenLW

- `ADVV`: Unix time in seconds at advertisement-session startup. It therefore changes whenever the advertised sources change, prompting other devices to reread the list.
- `BAST = 0`, `BASM = 1`: OpenLW provides no return stream.
- `SHAB = 0`, `FASM = 1`, `STPL = 0`.

## OpenLW transmission timing

| Event | Delay |
|---|---|
| Full advertisement | At startup, then 1 s ± 0.5 s later |
| Short advertisement | Every 20 s ± 5 s |
| Periodic full advertisement | After 8 short advertisements |
| Full-advertisement pages | 100 ms ± 50 ms between pages |

Observed on a Livewire device: short advertisement approximately every 10 s.

## Discovery

- A terminal is identified by its IP address (`INIP`).
- A full advertisement with a new `ADVV` replaces the terminal's source list; pages sharing an `ADVV` accumulate.
- A short advertisement refreshes only the terminal.
- A terminal silent for 75 s (three missed short advertisements) is removed.
- **Full-advertisement request** (OpenLW choice): when a short advertisement arrives from a terminal whose source list is unknown, OpenLW sends this unicast request to its `UDPC` port at most once every 5 s:

```
envelope: 03 00 02 07 | sequence | 00 × 8
message 'READ', 1 entry: 'ADVD' u8 = 1
```

Device responses to this request are not established ([open-questions.md](open-questions.md)). Otherwise, the list arrives with the next periodic full advertisement.

## Other observed message

Some devices also send an `ADVT = 3` message approximately every 10 s: `TERM` reduced to `HWID`, followed by an `S001` entry with `PSID`, a `BUSY` tag (u64), and two entries tagged `0xFFFFFFFF` and `0xFFFFFFFE` (u64). Meaning: hypothesis (source occupancy). OpenLW ignores it.

Vectors: [vectors/adv_packets.json](vectors/adv_packets.json). Implementations: `daemon/lw-proto/src/{envelope,tlv,adv}.rs`, `tools/lw/advcodec.py`. Dissector: `tools/wireshark/livewire.lua`.
