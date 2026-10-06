#!/bin/sh
# Tests du daemon compilé pour Windows (x86_64-pc-windows-gnu), exécutés sous Wine dans un conteneur.
# Wine n'énumère pas les cartes réseau du conteneur : les tests réseau (interfaces, multicast) ne
# tournent que sur un vrai Windows (CI). Un seul thread de test : Rosetta (OrbStack) refuse certains
# accès de Wine en parallèle.
#   tools/ci/test-wine.sh            depuis la racine du dépôt
set -eu
cd "$(dirname "$0")/../.."
docker build -q --platform linux/amd64 -t openlw-wine -f tools/ci/wine.Dockerfile tools/ci >/dev/null
docker run --rm --platform linux/amd64 -v "$PWD":/src -v openlw-target-wine:/target \
    -v openlw-cargo-amd64:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target -w /src/daemon openlw-wine sh -c '
    set -e
    T="--target x86_64-pc-windows-gnu"
    cargo test $T -p lw-sys -p lw-proto -- --test-threads=1
    cargo test $T -p lw-daemon --lib -- --test-threads=1 --skip iface::tests::loopback_is_listed
    cargo test $T -p lw-daemon --test device -- --test-threads=1'
