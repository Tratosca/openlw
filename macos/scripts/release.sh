#!/bin/sh
# Signed and notarized release, one package per architecture, stapled, with SHA-256 sums:
#   build/OpenLW-<version>-arm64.pkg   Apple Silicon, macOS 11+
#   build/OpenLW-<version>-x86_64.pkg  Intel, macOS 10.13+
# Both are cross-built on one Mac (universal build, then thinned by macos/installer/build-pkg.sh).
#   macos/scripts/release.sh             both architectures
#   ARCHS=arm64 macos/scripts/release.sh one of them
#
# Signing identities (looked up in the keychain when unset):
#   APP_SIGN_ID        "Developer ID Application: …" (daemon, HAL plugin, app, embedded Swift runtime)
#   INSTALLER_SIGN_ID  "Developer ID Installer: …"   (package)
#   KEYCHAIN           keychain holding both identities (CI: temporary keychain)
# Notarization (App Store Connect API key, "Developer" role), one of:
#   NOTARY_PROFILE     profile saved by `xcrun notarytool store-credentials`
#   NOTARY_KEY, NOTARY_KEY_ID, NOTARY_ISSUER   path to the .p8 key, key ID, issuer ID
# SKIP_NOTARIZE=1 stops after signing (local check without submitting to Apple).
# Notarization reports: build/notary-<arch>.json, build/notary-log-<arch>.json.
#
# Hardened runtime and secure timestamp on every executable; no entitlement is needed
# (no audio input, no JIT, no unsigned library loading).
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

ARCHS=${ARCHS:-arm64 x86_64}
find_identity() {
    # shellcheck disable=SC2086
    security find-identity -v -p "$1" ${KEYCHAIN:-} | sed -n "s/.*\"\($2: .*\)\"$/\1/p" | head -n 1
}
APP_SIGN_ID=${APP_SIGN_ID:-$(find_identity codesigning "Developer ID Application")}
INSTALLER_SIGN_ID=${INSTALLER_SIGN_ID:-$(find_identity basic "Developer ID Installer")}
[ -n "$APP_SIGN_ID" ] || { echo "\"Developer ID Application\" identity not found" >&2; exit 1; }
[ -n "$INSTALLER_SIGN_ID" ] || { echo "\"Developer ID Installer\" identity not found" >&2; exit 1; }

if [ "${SKIP_NOTARIZE:-0}" != 1 ]; then
    if [ -n "${NOTARY_PROFILE:-}" ]; then
        NOTARY_AUTH="--keychain-profile $NOTARY_PROFILE"
        [ -n "${KEYCHAIN:-}" ] && NOTARY_AUTH="$NOTARY_AUTH --keychain $KEYCHAIN"
    elif [ -n "${NOTARY_KEY:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then
        NOTARY_AUTH="--key $NOTARY_KEY --key-id $NOTARY_KEY_ID --issuer $NOTARY_ISSUER"
    else
        echo "notarization: set NOTARY_PROFILE, or NOTARY_KEY, NOTARY_KEY_ID and NOTARY_ISSUER" >&2
        exit 1
    fi
fi

echo "== Build"
macos/scripts/build-all.sh

VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" macos/app/Info.plist)
for arch in $ARCHS; do
    echo "== Package $arch: $APP_SIGN_ID, $INSTALLER_SIGN_ID"
    APP_SIGN_ID="$APP_SIGN_ID" INSTALLER_SIGN_ID="$INSTALLER_SIGN_ID" macos/installer/build-pkg.sh "$arch"
    pkgutil --check-signature "build/OpenLW-$VERSION-$arch.pkg" | head -n 2
done

if [ "${SKIP_NOTARIZE:-0}" = 1 ]; then
    echo "OK (signed, not notarized): $ROOT/build/OpenLW-$VERSION-*.pkg"
    exit 0
fi

for arch in $ARCHS; do
    PKG=build/OpenLW-$VERSION-$arch.pkg
    echo "== Notarization $arch"
    # shellcheck disable=SC2086
    xcrun notarytool submit "$PKG" $NOTARY_AUTH --wait --timeout 1h --output-format json >"build/notary-$arch.json" || true
    ID=$(plutil -extract id raw -o - "build/notary-$arch.json" 2>/dev/null || true)
    STATUS=$(plutil -extract status raw -o - "build/notary-$arch.json" 2>/dev/null || true)
    if [ -n "$ID" ]; then
        # shellcheck disable=SC2086
        xcrun notarytool log "$ID" $NOTARY_AUTH "build/notary-log-$arch.json" || true
    fi
    if [ "$STATUS" != Accepted ]; then
        echo "notarization rejected (status: ${STATUS:-unknown}), see build/notary-$arch.json and build/notary-log-$arch.json" >&2
        exit 1
    fi

    echo "== Stapling $arch"
    xcrun stapler staple "$PKG"
    xcrun stapler validate "$PKG"
    spctl --assess --type install --verbose=2 "$PKG"
    (cd build && shasum -a 256 "OpenLW-$VERSION-$arch.pkg" >"OpenLW-$VERSION-$arch.pkg.sha256")
    echo "OK: $ROOT/$PKG"
done
