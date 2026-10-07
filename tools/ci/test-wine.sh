#!/bin/sh
# Windows-compiled daemon tests run under Wine in a container.
#   tools/ci/test-wine.sh          Windows ARM64 (aarch64-pc-windows-gnullvm, native Wine on arm64 host),
#                                  then audio driver (windows/driver) with its test host
#   tools/ci/test-wine.sh x64      Windows x64 (x86_64-pc-windows-gnu; emulated Wine on arm64 host)
#
# Limitations: Wine does not enumerate container network adapters; network tests (interfaces,
# multicast) run only on real Windows (CI); MMCSS, tokens, and ACLs are merely emulated.
# Under amd64 emulation (Rosetta, OrbStack), use one test thread: Wine fails with parallel tests.
set -eu
cd "$(dirname "$0")/../.."
case "${1:-arm64}" in
    arm64)
        IMAGE=openlw-wine-arm64 PLATFORM=linux/arm64 TARGET=aarch64-pc-windows-gnullvm
        FILE=tools/ci/wine-arm64.Dockerfile VOL=openlw-target-winarm THREADS=""
        ;;
    x64)
        IMAGE=openlw-wine PLATFORM=linux/amd64 TARGET=x86_64-pc-windows-gnu
        FILE=tools/ci/wine.Dockerfile VOL=openlw-target-wine THREADS="--test-threads=1"
        ;;
    *)
        echo "usage: $0 [arm64|x64]" >&2
        exit 2
        ;;
esac
docker build -q --platform "$PLATFORM" -t "$IMAGE" -f "$FILE" tools/ci >/dev/null
docker run --rm --platform "$PLATFORM" -v "$PWD":/src -v "$VOL":/target \
    -v "openlw-cargo-${PLATFORM#linux/}":/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target \
    -w /src/daemon "$IMAGE" sh -c "
    set -e
    T='--target $TARGET'
    cargo test \$T -p lw-sys -p lw-proto -- $THREADS
    cargo test \$T -p lw-daemon --lib -- $THREADS --skip iface::tests::loopback_is_listed
    cargo test \$T -p lw-daemon --test device -- $THREADS"

# Windows audio driver (ARM64 only: test host requires native execution).
[ "${1:-arm64}" = arm64 ] || exit 0
docker run --rm --platform "$PLATFORM" -v "$PWD":/src -v "$VOL":/target -v openlw-driver-build:/build \
    -v "openlw-cargo-${PLATFORM#linux/}":/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target \
    -w /src "$IMAGE" timeout 600 sh -c "
    set -e
    windows/driver/fetch-sdk.sh
    cmake -S windows/driver -B /build/arm64 -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_TOOLCHAIN_FILE=/src/windows/driver/cmake/llvm-mingw-aarch64.cmake >/dev/null
    cmake --build /build/arm64 >/dev/null
    (cd daemon && cargo build -q --target $TARGET -p lw-daemon)
    printf '{\"iface\":\"auto\",\"advertise\":false,\"device\":{\"channels_to_net\":2,\"channels_from_net\":2,\"loopback\":true}}' > /tmp/cfg.json
    wine /target/$TARGET/debug/lw-daemon.exe run --config 'Z:\\tmp\\cfg.json' --control >/tmp/daemon.log 2>&1 &
    sleep 3
    DLL='Z:\\build\\arm64\\OpenLWDriver.dll'
    wine /build/arm64/openlw-driver-test.exe \$DLL
    wine /build/arm64/openlw-driver-test.exe --hold 6 \$DLL >/dev/null 2>&1 &
    sleep 3
    wine /build/arm64/openlw-driver-test.exe --expect-busy \$DLL"
