# Linux GTK app (linux/app) build and preview: Ubuntu 24.04 is the oldest supported desktop
# (GTK 4.14, libadwaita 1.5, PipeWire 1.0). The app is rendered with the GTK Broadway backend
# and viewed in a browser; the daemon publishes its PipeWire nodes in the same container.
#   docker build --platform linux/arm64 -t openlw-gtk -f tools/ci/linux-gtk.Dockerfile tools/ci
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl build-essential clang pkg-config \
       libgtk-4-dev libadwaita-1-dev libgtk-4-bin \
       libpipewire-0.3-dev libspa-0.2-dev \
       pipewire pipewire-bin wireplumber dbus dbus-x11 \
       adwaita-icon-theme fonts-cantarell fonts-dejavu-core python3 \
       iproute2 dpkg-dev file desktop-file-utils appstream \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
       --component clippy,rustfmt \
    && cargo install --locked cargo-deb \
    && rm -rf /usr/local/cargo/registry
# WirePlumber 0.4 stops when its Bluetooth monitor cannot reach logind (absent in containers).
RUN mkdir -p /root/.config/wireplumber/bluetooth.lua.d \
    && echo "-- Bluetooth monitor disabled (no logind in containers)" \
       > /root/.config/wireplumber/bluetooth.lua.d/90-enable-all.lua
ENV XDG_RUNTIME_DIR=/run/user/0 LANG=C.UTF-8
