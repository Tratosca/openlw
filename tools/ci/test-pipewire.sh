#!/bin/sh
# Test Linux daemon PipeWire nodes in a container without sound card (dummy driver):
# - daemon started before PipeWire: nodes appear once the server is up (retry);
# - daemon internal loopback: sine played to “OpenLW Out”, recorded from “OpenLW In”;
# - PipeWire restarted: nodes are published again (reconnection);
# - multi layout: one node per device (openlw_in_n / openlw_out_n), loopback Out 2 -> In 2,
#   node renamed after a patch (ADR 0010).
#   tools/ci/test-pipewire.sh            arm64 host (native); PLATFORM=linux/amd64 for x86_64
set -eu
cd "$(dirname "$0")/../.."
PLATFORM=${PLATFORM:-linux/arm64}
docker build -q --platform "$PLATFORM" -t openlw-pipewire -f tools/ci/linux-pipewire.Dockerfile tools/ci >/dev/null
docker run --rm --platform "$PLATFORM" -v "$PWD":/src -v "openlw-target-pw-${PLATFORM#linux/}":/target \
    -v "openlw-cargo-${PLATFORM#linux/}":/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target \
    -w /src/daemon openlw-pipewire timeout 300 sh -c '
set -e
cargo build -q -p lw-daemon
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --fork --nopidfile
start_pipewire() {
    pipewire >>/tmp/pipewire.log 2>&1 &
    sleep 1
    wireplumber >>/tmp/wireplumber.log 2>&1 &
    sleep 2
}
nodes() { pw-cli ls Node 2>/dev/null | grep -c -E "node.name = \"openlw_(in|out)\"" || true; }
# status.audio_nodes reported by the daemon (true: nodes published, false: PipeWire unreachable).
audio_nodes() {
    want=$1
    for i in $(seq 1 30); do
        /target/debug/lw-daemon ctl --endpoint unix:/tmp/openlw.sock status 2>/dev/null \
            | grep -q "\"audio_nodes\": $want" && { echo "ok   status.audio_nodes = $want"; return 0; }
        sleep 0.5
    done
    echo "FAIL: status.audio_nodes is not $want"; exit 1
}
wait_nodes() {
    for i in $(seq 1 30); do [ "$(nodes)" = 2 ] && return 0; sleep 0.5; done
    echo "FAIL: nodes missing"; cat /tmp/daemon.log; exit 1
}

echo "--- daemon started before PipeWire"
printf "{\"iface\":\"auto\",\"advertise\":false,\"device\":{\"channels_to_net\":2,\"channels_from_net\":2,\"loopback\":true}}" >/tmp/cfg.json
/target/debug/lw-daemon run --config /tmp/cfg.json --control unix:/tmp/openlw.sock >/tmp/daemon.log 2>&1 &
sleep 2
start_pipewire
wait_nodes
echo "ok   nodes published after PipeWire started"
audio_nodes true

echo "--- audio"
cat >/tmp/check_rec.py <<EOF
import struct, wave, sys
with wave.open("/tmp/rec.wav", "rb") as w:
    n = w.getnframes(); data = w.readframes(n)
samples = struct.unpack("<%dh" % (len(data) // 2), data)
left, right = samples[0::2], samples[1::2]
active = [(l, r) for l, r in zip(left, right) if l != 0]
peak = max((abs(l) for l, _ in active), default=0)
mismatch = sum(1 for l, r in active if abs(l + r) > 1)
ok = len(active) > 48000 and 15800 < peak < 16600 and mismatch == 0
label = sys.argv[1] if len(sys.argv) > 1 else "recorded"
print("%s %s: %d frames, %d active, peak %d (expected ~16384), L/R mismatches %d"
      % ("ok  " if ok else "FAIL", label, n, len(active), peak, mismatch))
sys.exit(0 if ok else 1)
EOF
python3 - <<EOF
import math, struct, wave
with wave.open("/tmp/sine.wav", "wb") as w:
    w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
    frames = bytearray()
    for n in range(48000 * 4):
        s = int(16384 * math.sin(2 * math.pi * 1000 * n / 48000))
        frames += struct.pack("<hh", s, -s)
    w.writeframes(bytes(frames))
EOF
pw-record --target openlw_in --format s16 --rate 48000 --channels 2 /tmp/rec.wav &
REC=$!
sleep 0.5
pw-play --target openlw_out /tmp/sine.wav &
PLAY=$!
sleep 3
kill $REC 2>/dev/null || true
wait $PLAY 2>/dev/null || true
python3 /tmp/check_rec.py "Out -> In"

echo "--- PipeWire restart"
pkill wireplumber || true
pkill -x pipewire || true
sleep 1
[ "$(nodes)" = 0 ] || true
audio_nodes false
start_pipewire
wait_nodes
echo "ok   nodes published again after PipeWire restart"
audio_nodes true

echo "--- multi layout"
ctl() { /target/debug/lw-daemon ctl --endpoint unix:/tmp/openlw.sock "$@"; }
multi_nodes() { pw-cli ls Node 2>/dev/null | grep -c -E "node.name = \"openlw_(in|out)_[12]\"" || true; }
ctl set-channels --to-net 4 --from-net 4 | grep -q "\"ok\": true"
ctl set-layout multi | grep -q "\"ok\": true"
for i in $(seq 1 30); do [ "$(multi_nodes)" = 4 ] && break; sleep 0.5; done
[ "$(multi_nodes)" = 4 ] || { echo "FAIL: multi nodes missing"; pw-cli ls Node; cat /tmp/daemon.log; exit 1; }
echo "ok   four nodes: openlw_in_1, openlw_in_2, openlw_out_1, openlw_out_2"
pw-cli ls Node | grep -q "node.description = \"OpenLW In 2\"" || { echo "FAIL: generic name"; exit 1; }
echo "ok   generic name OpenLW In 2"
pw-record --target openlw_in_2 --format s16 --rate 48000 --channels 2 /tmp/rec.wav &
REC=$!
sleep 0.5
pw-play --target openlw_out_2 /tmp/sine.wav &
PLAY=$!
sleep 3
kill $REC 2>/dev/null || true
wait $PLAY 2>/dev/null || true
python3 /tmp/check_rec.py "Out 2 -> In 2"
node_id() { pw-dump 2>/dev/null | python3 -c "import json,sys; print(next((o[\"id\"] for o in json.load(sys.stdin) if o.get(\"type\",\"\").endswith(\":Node\") and o.get(\"info\",{}).get(\"props\",{}).get(\"node.name\")==sys.argv[1]), \"\"))" "$1"; }
ID_BEFORE=$(node_id openlw_out_1)
ID_IN=$(node_id openlw_in_2)
ctl patch-out --device 1 --from 1,2 --channel 4001 --name "PC PGM" | grep -q "\"ok\": true"
# Node info (pw-dump) carries live properties; pw-cli ls shows the registry global, fixed at creation.
renamed() { pw-dump 2>/dev/null | python3 -c "import json,sys; sys.exit(0 if any(o.get(\"info\",{}).get(\"props\",{}).get(\"node.description\")==sys.argv[1] for o in json.load(sys.stdin)) else 1)" "OpenLW Out - PC PGM (ch. 4001)"; }
for i in $(seq 1 30); do renamed && break; sleep 0.5; done
renamed || { echo "FAIL: node not renamed"; pw-dump | grep -E "node.description"; cat /tmp/daemon.log; exit 1; }
echo "ok   node renamed after a patch: OpenLW Out - PC PGM (ch. 4001)"
[ -n "$ID_BEFORE" ] && [ "$(node_id openlw_out_1)" = "$ID_BEFORE" ] && [ "$(node_id openlw_in_2)" = "$ID_IN" ] \
    || { echo "FAIL: nodes recreated by the rename ($ID_BEFORE -> $(node_id openlw_out_1))"; exit 1; }
echo "ok   renamed live: node ids unchanged ($ID_BEFORE, $ID_IN)"
ctl set-layout duplex | grep -q "\"ok\": true"
wait_nodes
echo "ok   back to duplex: openlw_in, openlw_out"

echo "--- daemon log"; cat /tmp/daemon.log
echo "SUCCESS"'
