#!/bin/sh
# Construit le daemon (binaire universel signé ad hoc), le plugin HAL et l'app. Sans sudo.
# Sorties : build/lw-daemon, macos/plugin/build/OpenLW.driver, macos/app/build/OpenLW.app
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
export PATH="$HOME/.cargo/bin:$PATH"
(cd daemon && cargo build --release --target x86_64-apple-darwin && cargo build --release --target aarch64-apple-darwin)
mkdir -p build
lipo -create -output build/lw-daemon \
    daemon/target/x86_64-apple-darwin/release/lw-daemon daemon/target/aarch64-apple-darwin/release/lw-daemon
codesign --force --sign - --identifier fr.francois-brille.openlw.daemon build/lw-daemon
make -C macos/plugin
make -C macos/app
echo "OK : $ROOT/build/lw-daemon, $ROOT/plugin/build/OpenLW.driver, $ROOT/app/build/OpenLW.app"
