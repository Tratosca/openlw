#!/bin/sh
# Tests du daemon compilé pour Windows, exécutés sous Wine dans un conteneur.
#   tools/ci/test-wine.sh          Windows ARM64 (aarch64-pc-windows-gnullvm, Wine natif sur hôte arm64)
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
