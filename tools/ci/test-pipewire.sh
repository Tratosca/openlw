#!/bin/sh
# Test Linux daemon PipeWire nodes in a container without sound card (dummy driver):
# - daemon started before PipeWire: nodes appear once the server is up (retry);
# - daemon internal loopback: sine played to “OpenLW Out”, recorded from “OpenLW In”;
# - PipeWire restarted: nodes are published again (reconnection).
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
wait_nodes() {
    for i in $(seq 1 30); do [ "$(nodes)" = 2 ] && return 0; sleep 0.5; done
    echo "ÉCHEC : nœuds absents"; cat /tmp/daemon.log; exit 1
}

echo "--- daemon started before PipeWire"
printf "{\"iface\":\"auto\",\"advertise\":false,\"device\":{\"channels_to_net\":2,\"channels_from_net\":2,\"loopback\":true}}" >/tmp/cfg.json
/target/debug/lw-daemon run --config /tmp/cfg.json --control unix:/tmp/openlw.sock >/tmp/daemon.log 2>&1 &
sleep 2
start_pipewire
wait_nodes
echo "ok   nodes published after PipeWire started"

echo "--- audio"
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
python3 - <<EOF
import struct, wave, sys
with wave.open("/tmp/rec.wav", "rb") as w:
    n = w.getnframes(); data = w.readframes(n)
samples = struct.unpack("<%dh" % (len(data) // 2), data)
left, right = samples[0::2], samples[1::2]
active = [(l, r) for l, r in zip(left, right) if l != 0]
peak = max((abs(l) for l, _ in active), default=0)
mismatch = sum(1 for l, r in active if abs(l + r) > 1)
ok = len(active) > 48000 and 15800 < peak < 16600 and mismatch == 0
print("%s recorded %d frames, %d active, peak %d (expected ~16384), L/R mismatches %d"
      % ("ok  " if ok else "ÉCHEC", n, len(active), peak, mismatch))
sys.exit(0 if ok else 1)
EOF

echo "--- PipeWire restart"
pkill wireplumber || true
pkill -x pipewire || true
sleep 1
[ "$(nodes)" = 0 ] || true
start_pipewire
wait_nodes
echo "ok   nodes published again after PipeWire restart"

echo "--- daemon log"; cat /tmp/daemon.log
echo "SUCCÈS"'
