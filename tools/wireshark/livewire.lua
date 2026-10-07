-- Livewire: based on docs/protocol/ specification.
-- All buffer accesses are bounded, including TlvMsg submessages.
local adv = Proto("lwadv", "Livewire Envelope / TlvMsg")
local clock = Proto("lwclock", "Livewire Clock")
local audio = Proto("lwrtp", "Livewire RTP Audio")
local f = {
    layer = ProtoField.uint8("lwadv.layer", "Layer", base.DEC),
    kind = ProtoField.uint8("lwadv.kind", "Envelope Type", base.HEX,
        {[0]="DATAGRAM", [0x4d]="MESSAGE (M)", [0x41]="ACK (A)", [0x4e]="NACK (N)"}),
    msgver = ProtoField.uint8("lwadv.cmsg_version", "TlvMsg Version", base.DEC, nil, 0x7f),
    version = ProtoField.uint8("lwadv.envelope_version", "Envelope Version", base.DEC),
    seq = ProtoField.uint32("lwadv.seq", "Sequence Number", base.DEC),
    port = ProtoField.uint16("lwadv.reply_port", "Reply Port", base.DEC),
    lockid = ProtoField.uint16("lwadv.lock_id", "LockID", base.HEX),
    locktid = ProtoField.uint32("lwadv.lock_tid", "LockTID", base.HEX),
    id = ProtoField.string("lwadv.message_id", "Message FourCC"),
    count = ProtoField.uint16("lwadv.count", "Item Count", base.DEC),
    tag = ProtoField.string("lwadv.tag", "Tag FourCC"),
    typ = ProtoField.uint8("lwadv.type", "TlvMsg Type", base.DEC),
    u8 = ProtoField.uint8("lwadv.u8", "u8 Value", base.DEC),
    u16 = ProtoField.uint16("lwadv.u16", "u16 Value", base.DEC),
    u32 = ProtoField.uint32("lwadv.u32", "u32 Value", base.DEC),
    u64 = ProtoField.uint64("lwadv.u64", "u64 Value", base.DEC),
    length = ProtoField.uint16("lwadv.length", "Length / Element Count", base.DEC),
    bytes = ProtoField.bytes("lwadv.bytes", "Bytes"),
    text = ProtoField.string("lwadv.text", "String"),
    channel = ProtoField.uint32("lwadv.channel", "Livewire Channel", base.DEC),
    ip = ProtoField.ipv4("lwadv.ip", "IPv4 Address"),
    advt = ProtoField.uint8("lwadv.advt", "Advertisement Type", base.DEC, {[1]="full", [2]="short"}),
}
adv.fields = f
local malformed = {}
for _, proto in ipairs({adv, clock, audio}) do
    local e = ProtoExpert.new(proto.name .. ".malformed", "Truncated or malformed packet",
        expert.group.MALFORMED, expert.severity.ERROR)
    proto.experts = {e}
    malformed[proto.name] = e
end
local function bad(tree, proto, reason)
    tree:add_proto_expert_info(malformed[proto.name], reason)
    return nil
end
local function need(tree, proto, offset, size, limit)
    if offset + size > limit then
        return bad(tree, proto, string.format("Data truncated at offset %d: %d bytes required, %d available",
            offset, size, math.max(0, limit - offset)))
    end
    return true
end
local function fourcc(range)
    return (range:string():gsub("[^ -~]", function(c) return string.format("\\x%02X", c:byte()) end))
end
local function printable(range)
    -- Fixed-size names may be padded with trailing NULs.
    local s = range:string():gsub("%z+$", "")
    if not s:find("[^ -~]") then return s end
end
local sizes = {[1]=4, [7]=1, [8]=2, [9]=8}
local fields = {[1]=f.u32, [7]=f.u8, [8]=f.u16, [9]=f.u64}
local function message(tvb, offset, limit, parent, depth, summary, context)
    -- Root is level 1: at most eight total levels.
    if depth > 8 then return bad(parent, adv, "TlvMsg depth greater than 8") end
    if not need(parent, adv, offset, 6, limit) then return nil end
    local start = offset
    local id = fourcc(tvb(offset, 4))
    local node = parent:add(f.id, tvb(offset, 4), id)
    node:add(f.count, tvb(offset + 4, 2))
    local count = tvb(offset + 4, 2):uint()
    if depth == 1 then summary.id = id end
    offset = offset + 6
    for _ = 1, count do
        if not need(node, adv, offset, 5, limit) then return nil end
        local item_start = offset
        local tag = fourcc(tvb(offset, 4))
        local typ = tvb(offset + 4, 1):uint()
        local item = node:add(f.tag, tvb(offset, 4), tag)
        item:add(f.typ, tvb(offset + 4, 1))
        offset = offset + 5
        if sizes[typ] then
            local size = sizes[typ]
            if not need(item, adv, offset, size, limit) then return nil end
            local range = tvb(offset, size)
            item:add(fields[typ], range)
            if typ ~= 9 then
                local value = range:uint()
                item:append_text(" = " .. value)
                if tag == "PSID" or tag == "LPID" then
                    item:add(f.channel, range, value % 32768)
                elseif (tag == "FSID" or tag == "BSID" or tag == "INIP") and size == 4 then
                    item:add(f.ip, range)
                elseif tag == "ADVT" then
                    item:add(f.advt, value)
                    if depth == 1 then summary.advt = value end
                elseif tag == "NUMS" and context == "TERM" then summary.nums = value end
            end
            offset = offset + size
        elseif typ >= 2 and typ <= 6 then
            if not need(item, adv, offset, 2, limit) then return nil end
            item:add(f.length, tvb(offset, 2))
            local n = tvb(offset, 2):uint()
            offset = offset + 2
            local size = n * (typ == 4 and 2 or typ == 5 and 4 or 1)
            if not need(item, adv, offset, size, limit) then return nil end
            if typ == 6 then
                if depth == 1 and tag:match("^S%d%d%d$") then
                    table.insert(summary.sources, tag)
                end
                local finish = message(tvb, offset, offset + size, item, depth + 1, summary, tag)
                if not finish then return nil end
                if finish ~= offset + size then return bad(item, adv, "Trailing bytes in nested TlvMsg") end
            elseif typ == 4 or typ == 5 then
                local width = typ == 4 and 2 or 4
                for pos = offset, offset + size - 1, width do
                    item:add(typ == 4 and f.u16 or f.u32, tvb(pos, width))
                end
            else
                local range = tvb(offset, size)
                item:add(f.bytes, range)
                local s = printable(range)
                if s then
                    item:add(f.text, range, s)
                    item:append_text(' = "' .. s .. '"')
                    if tag == "ATRN" and context == "TERM" then summary.name = s end
                end
            end
            offset = offset + size
        else
            return bad(item, adv, "Unknown or unserializable TlvMsg type: " .. typ)
        end
        item:set_len(offset - item_start)
    end
    node:set_len(offset - start)
    return offset
end
function adv.dissector(tvb, pinfo, tree)
    pinfo.cols.protocol = "LWADV"
    pinfo.cols.info = "Truncated Envelope"
    local node = tree:add(adv, tvb())
    local size = tvb:len()
    if not need(node, adv, 0, 16, size) then return end
    for _, v in ipairs({{f.layer,0,1},{f.kind,1,1},{f.msgver,2,1},{f.version,3,1},
        {f.seq,4,4},{f.port,8,2},{f.lockid,10,2},{f.locktid,12,4}}) do
        node:add(v[1], tvb(v[2], v[3]))
    end
    local kind = tvb(1,1):uint()
    if size == 16 and (kind == 0x41 or kind == 0x4e) then
        pinfo.cols.info = kind == 0x41 and "Envelope ACK" or "Envelope NACK"
        return
    end
    local summary = {sources={}}
    local finish = message(tvb, 16, size, node, 1, summary)
    if finish and finish ~= size then bad(node, adv, "Trailing bytes after TlvMsg") end
    if summary.id == "NEST" and (summary.advt == 1 or summary.advt == 2) then
        pinfo.cols.info = string.format("ADV %s %s NUMS=%s sources=%s",
            summary.advt == 1 and "full" or "short", summary.name or "<none>",
            tostring(summary.nums or "?"), #summary.sources > 0 and table.concat(summary.sources, ",") or "<none>")
    else pinfo.cols.info = summary.id or "Truncated TlvMsg" end
end

-- Shared RTP decoding, accounting for CSRCs, extension, padding.
local function rtp_fields(prefix)
    return {
        version=ProtoField.uint8(prefix..".version", "RTP Version", base.DEC, nil, 0xc0),
        pt=ProtoField.uint8(prefix..".pt", "Payload Type", base.DEC, nil, 0x7f),
        seq=ProtoField.uint16(prefix..".seq", "RTP Sequence Number", base.DEC),
        timestamp=ProtoField.uint32(prefix..".timestamp", "RTP Timestamp", base.DEC),
        ssrc=ProtoField.uint32(prefix..".ssrc", "SSRC", base.HEX),
        extra=ProtoField.bytes(prefix..".header_extra", "RTP CSRC / Extension"),
        padding=ProtoField.bytes(prefix..".padding", "RTP Padding"),
        payload=ProtoField.bytes(prefix..".payload", "Raw RTP Payload"),
    }
end
local c, a = rtp_fields("lwclock"), rtp_fields("lwrtp")
c.master = ProtoField.bytes("lwclock.master_id", "Master Identifier (bytes 26–29)")
c.ext_profile = ProtoField.uint16("lwclock.ext_profile", "Extension Profile", base.HEX)
c.ext_ok = ProtoField.bool("lwclock.ext_ok", "FA1A/20-word Extension", base.NONE, {"yes", "no"})
c.clock_seq = ProtoField.uint32("lwclock.clock_seq", "Clock Sequence Number", base.DEC)
c.clock_type = ProtoField.string("lwclock.type", "Message Type (A/B)")
a.channel = ProtoField.uint32("lwrtp.channel", "Livewire Channel", base.DEC)
a.kind = ProtoField.string("lwrtp.kind", "Stream Kind")
a.channels = ProtoField.uint8("lwrtp.channels", "Audio Channel Count", base.DEC)
a.samples = ProtoField.double("lwrtp.samples", "Samples per Packet (L24)")
a.match = ProtoField.bool("lwrtp.ssrc_matches_dst", "SSRC == ip.dst", base.NONE, {"yes", "no"})
clock.fields, audio.fields = c, a
local function rtp(tvb, node, proto, rf)
    local size = tvb:len()
    if not need(node, proto, 0, 12, size) then return nil end
    node:add(rf.version, tvb(0,1)); node:add(rf.pt, tvb(1,1))
    node:add(rf.seq, tvb(2,2)); node:add(rf.timestamp, tvb(4,4)); node:add(rf.ssrc, tvb(8,4))
    local flags = tvb(0,1):uint()
    if math.floor(flags / 64) ~= 2 then return bad(node, proto, "RTP version is not 2") end
    local offset = 12 + (flags % 16) * 4
    if not need(node, proto, 12, offset - 12, size) then return nil end
    if math.floor(flags / 16) % 2 == 1 then
        if not need(node, proto, offset, 4, size) then return nil end
        local length = 4 + tvb(offset + 2, 2):uint() * 4
        if not need(node, proto, offset, length, size) then return nil end
        offset = offset + length
    end
    if offset > 12 then node:add(rf.extra, tvb(12, offset - 12)) end
    local padding = 0
    if math.floor(flags / 32) % 2 == 1 then
        padding = tvb(size - 1, 1):uint()
        if padding == 0 or padding > size - offset then return bad(node, proto, "Invalid RTP padding") end
        node:add(rf.padding, tvb(size - padding, padding))
    end
    local length = size - offset - padding
    node:add(rf.payload, tvb(offset, length))
    return length
end
function clock.dissector(tvb, pinfo, tree)
    pinfo.cols.protocol = "LWCLOCK"
    pinfo.cols.info = "Livewire clock"
    local node = tree:add(clock, tvb())
    if not rtp(tvb, node, clock, c) then return end
    -- Absolute offsets in UDP payload (docs/protocol/04-clock.md).
    local size = tvb:len()
    if size >= 24 then
        local profile, words = tvb(12,2):uint(), tvb(14,2):uint()
        node:add(c.ext_profile, tvb(12,2))
        node:add(c.ext_ok, tvb(12,4), profile == 0xFA1A and words == 0x14)
        node:add(c.clock_seq, tvb(16,4))
        local magic = tvb(20,4):bytes():tohex():lower()
        local kind = (magic == "0a00caba" and "A") or (magic == "0b00caba" and "B") or magic
        node:add(c.clock_type, tvb(20,4), kind)
        pinfo.cols.info = string.format("Livewire clock type %s seq=%u ts=%u%s", kind, tvb(16,4):uint(), tvb(4,4):uint(),
            (profile == 0xFA1A and words == 0x14) and "" or " (unexpected extension)")
    end
    if need(node, clock, 26, 4, size) then node:add(c.master, tvb(26,4)) end
end
function audio.dissector(tvb, pinfo, tree)
    pinfo.cols.protocol = "LWRTP"
    pinfo.cols.info = "Livewire RTP"
    local node = tree:add(audio, tvb())
    local length = rtp(tvb, node, audio, a)
    if not length then return end
    local b1,b2,b3,b4 = tostring(pinfo.dst):match("^(%d+)%.(%d+)%.(%d+)%.(%d+)$")
    b1,b2,b3,b4 = tonumber(b1),tonumber(b2),tonumber(b3),tonumber(b4)
    if b1 ~= 239 or (b2 ~= 192 and b2 ~= 193 and b2 ~= 196) then return end
    local channel = (b3 * 256 + b4) % 32768
    local channels = b2 == 196 and 8 or 2
    local kind = b2 == 196 and "surround" or b2 == 193 and "backfeed" or "stereo"
    local samples = length / (3 * channels)
    local equal = tvb(8,4):uint() == ((b1 * 256 + b2) * 256 + b3) * 256 + b4
    node:add(a.channel, channel):set_generated()
    node:add(a.kind, kind):set_generated()
    node:add(a.channels, channels):set_generated()
    node:add(a.samples, samples):set_generated()
    node:add(a.match, equal):set_generated()
    pinfo.cols.info = string.format("RTP L24 %s channel=%d samples=%g seq=%d SSRC==dst=%s",
        kind, channel, samples, tvb(2,2):uint(), equal and "yes" or "no")
end

-- Zero disables binding; out-of-range ports ignored.
adv.prefs.control_port = Pref.uint("Control UDP port", 4000, "1–65535; 0 disables")
adv.prefs.announce_port = Pref.uint("Advertisement UDP port", 4001, "1–65535; 0 disables")
clock.prefs.port = Pref.uint("Clock UDP port", 7000, "1–65535; 0 disables")
audio.prefs.port = Pref.uint("Audio UDP port", 5004, "1–65535; 0 disables")
local udp = DissectorTable.get("udp.port")
local function bind(proto, names)
    local previous = {}
    local function update()
        for port in pairs(previous) do udp:remove(port, proto) end
        previous = {}
        for _, name in ipairs(names) do
            local port = proto.prefs[name]
            if port > 0 and port <= 65535 and not previous[port] then
                udp:add(port, proto)
                previous[port] = true
            end
        end
    end
    proto.prefs_changed = update
    update()
end
bind(adv, {"control_port", "announce_port"})
bind(clock, {"port"})
bind(audio, {"port"})
