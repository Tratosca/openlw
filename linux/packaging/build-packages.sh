#!/bin/sh
# Build the Linux packages in containers:
# - openlw-daemon: user service and PipeWire nodes, no GUI dependency;
# - openlw: the GTK app, depends on openlw-daemon.
# .deb: daemon built on Debian 12 (oldest supported glibc and PipeWire), app on Ubuntu 24.04
# (libadwaita 1.4 or later). .rpm: both built on Fedora 43.
#   linux/packaging/build-packages.sh [deb|rpm|all]      PLATFORM=linux/amd64 for x86_64
# Output: linux/packaging/dist/
set -eu
cd "$(dirname "$0")/../.."
FORMAT=${1:-all}
PLATFORM=${PLATFORM:-linux/arm64}
ARCH=${PLATFORM#linux/}
OUT=linux/packaging/dist
mkdir -p "$OUT"

run() { # image, volume suffix, command
    docker run --rm --platform "$PLATFORM" -v "$PWD":/src -v "openlw-pkg-target-$2-$ARCH":/target \
        -v "openlw-pkg-cargo-$2-$ARCH":/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target \
        -w /src "$1" sh -ec "$3"
}

if [ "$FORMAT" = deb ] || [ "$FORMAT" = all ]; then
    docker build -q --platform "$PLATFORM" -t openlw-pipewire -f tools/ci/linux-pipewire.Dockerfile tools/ci >/dev/null
    docker build -q --platform "$PLATFORM" -t openlw-gtk -f tools/ci/linux-gtk.Dockerfile tools/ci >/dev/null
    run openlw-pipewire bookworm "
        cd daemon && cargo build --release -q -p lw-daemon
        cargo deb -q --no-build -p lw-daemon -o /src/$OUT/"
    run openlw-gtk noble "
        # No public homepage yet (no published repository): add <url type="homepage"> then.
        desktop-file-validate linux/packaging/fr.francois_brille.openlw.desktop
        appstreamcli validate --no-net --explain --override=url-homepage-missing=info linux/packaging/fr.francois_brille.openlw.metainfo.xml
        cd linux/app && cargo build --release -q
        cargo deb -q --no-build -o /src/$OUT/"
fi

if [ "$FORMAT" = rpm ] || [ "$FORMAT" = all ]; then
    docker build -q --platform "$PLATFORM" -t openlw-fedora -f tools/ci/linux-fedora.Dockerfile tools/ci >/dev/null
    run openlw-fedora fedora "
        (cd daemon && cargo build --release -q -p lw-daemon \
            && cargo generate-rpm -p lw-daemon --target-dir /target -o /src/$OUT/)
        cd linux/app && cargo build --release -q
        cargo generate-rpm --target-dir /target -o /src/$OUT/"
fi
ls -l "$OUT"
