# Linux daemon with PipeWire: build (pipewire-rs, bindgen) and test OpenLW nodes in
# a sound-card-free PipeWire graph (dummy driver), using WirePlumber for links.
#   docker build --platform linux/arm64 -t openlw-pipewire -f tools/ci/linux-pipewire.Dockerfile tools/ci
ARG DEBIAN=bookworm
FROM rust:1-${DEBIAN}
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       libpipewire-0.3-dev libspa-0.2-dev clang pkg-config \
       pipewire pipewire-bin wireplumber dbus \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt
# WirePlumber 0.4 stops when its Bluetooth monitor cannot reach logind (absent in containers):
# override the file that enables it.
RUN mkdir -p /root/.config/wireplumber/bluetooth.lua.d \
    && echo "-- Bluetooth monitor disabled (no logind in containers)" \
       > /root/.config/wireplumber/bluetooth.lua.d/90-enable-all.lua
ENV XDG_RUNTIME_DIR=/run/user/0
