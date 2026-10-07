#!/bin/sh
# Preview the Linux app (linux/app) in a browser: Ubuntu 24.04 container with PipeWire,
# the daemon (PipeWire nodes, control socket) and the app rendered by the GTK Broadway backend.
# A second container plays a neighboring Livewire device: two advertised sources, one of them
# with a standard RTP stream, to populate the input matrix. Multicast stays on the container
# bridge network.
#   tools/ci/preview-gtk.sh                                   then open http://localhost:8085
#   docker rm -f openlw-gtk-preview openlw-gtk-sources        to stop
set -eu
cd "$(dirname "$0")/../.."
PLATFORM=${PLATFORM:-linux/arm64}
PORT=${PORT:-8085}
docker build -q --platform "$PLATFORM" -t openlw-gtk -f tools/ci/linux-gtk.Dockerfile tools/ci >/dev/null
docker rm -f openlw-gtk-preview openlw-gtk-sources >/dev/null 2>&1 || true
docker run -d --name openlw-gtk-preview --platform "$PLATFORM" -p "127.0.0.1:$PORT:8085" \
    -v "$PWD":/src -v "openlw-gtk-target":/target -v "openlw-gtk-cargo":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target -w /src openlw-gtk sh -c '
set -e
(cd daemon && cargo build -q -p lw-daemon)
(cd linux/app && cargo build -q)
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --fork --nopidfile
pipewire >/tmp/pipewire.log 2>&1 &
sleep 1
wireplumber >/tmp/wireplumber.log 2>&1 &
mkdir -p /root/.config/openlw
/target/debug/lw-daemon run --config /root/.config/openlw/lw-daemon.json --init-config --control \
    >/tmp/daemon.log 2>&1 &
# Broadway reports no screen resolution: use the usual 96 dpi so sizes match a desktop.
mkdir -p /root/.config/gtk-4.0
printf "[Settings]\ngtk-xft-dpi=98304\n" >/root/.config/gtk-4.0/settings.ini
cd /src
gtk4-broadwayd --port 8085 --address 0.0.0.0 :5 >/tmp/broadway.log 2>&1 &
sleep 1
export GDK_BACKEND=broadway BROADWAY_DISPLAY=:5
/target/debug/openlw >/tmp/app.log 2>&1 &
echo READY
wait'
# Sources start once the daemon listens: full advertisements come only at startup, then every
# few minutes.
until docker logs openlw-gtk-preview 2>&1 | grep -q READY; do sleep 2; done
sleep 4
docker run -d --name openlw-gtk-sources --platform "$PLATFORM" -v "$PWD/tools/lw":/lw:ro -w /lw \
    openlw-gtk sh -c '
python3 emit_adv.py --iface eth0 --channel 101 --name "STUDIO A PGM" --terminal "studio-a" --seconds 86400 &
python3 emit_rtp.py --iface eth0 --channel 101 --mode standard --seconds 86400 --level-dbfs -14 &
python3 emit_adv.py --iface eth0 --channel 2204 --name "CODEC RETURN" --terminal "codec-1" --ip 192.168.215.250 --fast 3 --seconds 86400 &
wait' >/dev/null
echo "Preview: http://localhost:$PORT (logs: docker exec openlw-gtk-preview cat /tmp/app.log /tmp/daemon.log)"
