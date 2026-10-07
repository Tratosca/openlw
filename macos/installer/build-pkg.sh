#!/bin/sh
# Build installer build/OpenLW-<version>-<arch>.pkg for one architecture:
#   arm64   Apple Silicon, macOS 11 and later
#   x86_64  Intel, macOS 10.13 and later (embeds the Swift runtime for 10.13–10.14.3)
# Each package refuses to install on the other architecture.
#   macos/installer/build-pkg.sh arm64      after macos/scripts/build-all.sh (universal binaries, thinned here)
#   APP_SIGN_ID="Developer ID Application: …" INSTALLER_SIGN_ID="Developer ID Installer: …" \
#       macos/installer/build-pkg.sh x86_64    signed package (hardened runtime, secure timestamp)
#   (KEYCHAIN: keychain holding these identities, default search list otherwise)
# Unsigned packages carry ad hoc signed code. Signed, notarized release: macos/scripts/release.sh.
# Installed contents: see macos/installer/README.md.
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
ARCH=${1:-}
case "$ARCH" in
    arm64) MIN_OS=11.0 APPLE_SILICON=true ;;
    x86_64) MIN_OS=10.13 APPLE_SILICON=false ;;
    *) echo "usage: $0 arm64|x86_64" >&2; exit 2 ;;
esac
APPSRC="macos/app/build/OpenLW.app"
for f in build/lw-daemon macos/plugin/build/OpenLW.driver "$APPSRC"; do
    [ -e "$f" ] || { echo "missing: $f (run macos/scripts/build-all.sh first)" >&2; exit 1; }
done
VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" macos/app/Info.plist)
STAGE=build/pkg-$ARCH
rm -rf "$STAGE"
R="$STAGE/root"
APP="$R/Applications/OpenLW.app"
PLUGIN="$R/Library/Audio/Plug-Ins/HAL/OpenLW.driver"
DAEMON="$R/Library/Application Support/OpenLW/lw-daemon"
mkdir -p "$R/Library/Audio/Plug-Ins/HAL" "$R/Library/Application Support/OpenLW" "$R/Library/LaunchDaemons" "$R/Applications"
ditto macos/plugin/build/OpenLW.driver "$PLUGIN"
install -m 755 build/lw-daemon "$DAEMON"
install -m 755 macos/installer/payload/uninstall.sh "$R/Library/Application Support/OpenLW/uninstall.sh"
install -m 644 daemon/lw-daemon.default.json "$R/Library/Application Support/OpenLW/lw-daemon.default.json"
install -m 644 macos/launchd/fr.francois-brille.openlw.daemon.plist "$R/Library/LaunchDaemons/"
ditto "$APPSRC" "$APP"
# Swift runtime ships with macOS from 10.14.4: only the Intel build (10.13+) embeds it.
[ "$ARCH" = arm64 ] && rm -rf "$APP/Contents/Frameworks"
# Extended attributes removed (quarantine, etc.). Protected com.apple.provenance remains: pkgbuild records it
# as ._* entries that installer reapplies as an attribute.
xattr -cr "$R" 2>/dev/null || true

# Thin universal Mach-O files, then check that each holds the target architecture only.
find "$R" -type f | while read -r f; do
    file -b "$f" | grep -q "Mach-O" || continue
    archs=$(lipo -archs "$f")
    if [ "$archs" != "$ARCH" ] && lipo "$f" -verify_arch "$ARCH" 2>/dev/null; then
        lipo "$f" -thin "$ARCH" -output "$f.thin"
        chmod "$(stat -f %Lp "$f")" "$f.thin"
        mv "$f.thin" "$f"
        archs=$(lipo -archs "$f")
    fi
    [ "$archs" = "$ARCH" ] || { echo "unexpected architecture ($archs): $f" >&2; exit 1; }
done

# Thinning and runtime removal invalidate signatures: sign inside out, after staging.
KC_ARGS=""
[ -n "${KEYCHAIN:-}" ] && KC_ARGS="--keychain $KEYCHAIN"
sign() {
    if [ -n "${APP_SIGN_ID:-}" ]; then
        # shellcheck disable=SC2086
        codesign --force --options runtime --timestamp --sign "$APP_SIGN_ID" $KC_ARGS "$@"
    else
        codesign --force --sign - "$@"
    fi
}
if [ -d "$APP/Contents/Frameworks" ]; then
    find "$APP/Contents/Frameworks" -name '*.dylib' -type f | while read -r lib; do sign "$lib"; done
fi
sign "$APP"
sign "$PLUGIN"
sign --identifier fr.francois-brille.openlw.daemon "$DAEMON"
for f in "$APP" "$PLUGIN" "$DAEMON"; do
    codesign --verify --strict --deep "$f"
    if [ -n "${APP_SIGN_ID:-}" ]; then
        codesign --display --verbose=2 "$f" 2>&1 | grep -q "flags=.*runtime" || { echo "hardened runtime missing: $f" >&2; exit 1; }
    fi
done

# Non-relocatable bundles (installed at their path even if another copy exists), always replaced.
pkgbuild --analyze --root "$R" "$STAGE/component.plist" >/dev/null
n=$(/usr/libexec/PlistBuddy -c "Print" "$STAGE/component.plist" | grep -c "RootRelativeBundlePath")
i=0
while [ "$i" -lt "$n" ]; do
    for key in BundleIsRelocatable BundleIsVersionChecked; do
        /usr/libexec/PlistBuddy -c "Delete :$i:$key" "$STAGE/component.plist" 2>/dev/null || true
        /usr/libexec/PlistBuddy -c "Add :$i:$key bool false" "$STAGE/component.plist"
    done
    i=$((i + 1))
done
pkgbuild --root "$R" --component-plist "$STAGE/component.plist" --scripts macos/installer/scripts \
    --identifier fr.francois-brille.openlw --version "$VERSION" --install-location / \
    --ownership recommended "$STAGE/OpenLW-component.pkg"
sed -e "s/__VERSION__/$VERSION/" -e "s/__MIN_OS__/$MIN_OS/" -e "s/__APPLE_SILICON__/$APPLE_SILICON/" \
    macos/installer/distribution.xml >"$STAGE/distribution.xml"
OUT="build/OpenLW-$VERSION-$ARCH.pkg"
set --
if [ -n "${INSTALLER_SIGN_ID:-}" ]; then
    set -- --sign "$INSTALLER_SIGN_ID" --timestamp
    [ -n "${KEYCHAIN:-}" ] && set -- "$@" --keychain "$KEYCHAIN"
fi
productbuild --distribution "$STAGE/distribution.xml" --package-path "$STAGE" --resources macos/installer/resources \
    "$@" "$OUT"
echo "OK: $ROOT/$OUT"
