#!/bin/sh
# Tests du daemon compilé pour Windows, exécutés sous Wine dans un conteneur.
#   tools/ci/test-wine.sh          Windows ARM64 (aarch64-pc-windows-gnullvm, Wine natif sur hôte arm64),
#                                  puis pilote audio (windows/driver) avec son hôte de test
#   tools/ci/test-wine.sh x64      Windows x64 (x86_64-pc-windows-gnu ; sur hôte arm64, Wine émulé)
#
# Limites : Wine n'énumère pas les cartes réseau du conteneur, les tests réseau (interfaces,
# multicast) ne tournent que sur un vrai Windows (CI) ; MMCSS, jetons et ACL n'y sont qu'imités.
# Sous émulation amd64 (Rosetta, OrbStack), un seul thread de test : Wine y échoue en parallèle.
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
        echo "usage : $0 [arm64|x64]" >&2
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

# Pilote audio Windows (ARM64 seulement : l'hôte de test exige l'exécution native).
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
