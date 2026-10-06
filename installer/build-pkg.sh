#!/bin/sh
# Construit l'installeur build/OpenLW-<version>.pkg (Intel et Apple Silicon, macOS 10.13 et plus).
#   installer/build-pkg.sh            après scripts/build-all.sh
#   INSTALLER_SIGN_ID="Developer ID Installer: …" installer/build-pkg.sh   paquet signé
# Contenu posé : voir installer/README.md.
set -eu
cd "$(dirname "$0")/.."
ROOT=$(pwd)
APPSRC="app/build/OpenLW.app"
for f in build/lw-daemon plugin/build/OpenLW.driver "$APPSRC"; do
    [ -e "$f" ] || { echo "absent : $f (lancer d'abord scripts/build-all.sh)" >&2; exit 1; }
done
VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" app/Info.plist)
STAGE=build/pkg
rm -rf "$STAGE"
R="$STAGE/root"
mkdir -p "$R/Library/Audio/Plug-Ins/HAL" "$R/Library/Application Support/OpenLW" "$R/Library/LaunchDaemons" "$R/Applications"
ditto plugin/build/OpenLW.driver "$R/Library/Audio/Plug-Ins/HAL/OpenLW.driver"
install -m 755 build/lw-daemon "$R/Library/Application Support/OpenLW/lw-daemon"
install -m 755 installer/payload/uninstall.sh "$R/Library/Application Support/OpenLW/uninstall.sh"
install -m 644 installer/payload/lw-daemon.default.json "$R/Library/Application Support/OpenLW/lw-daemon.default.json"
install -m 644 daemon/launchd/fr.francois-brille.openlw.daemon.plist "$R/Library/LaunchDaemons/"
ditto "$APPSRC" "$R/Applications/OpenLW.app"
# Attributs étendus retirés (quarantaine…). com.apple.provenance, protégé, reste : pkgbuild l'enregistre
# en entrées ._* que l'installeur réapplique comme attribut.
xattr -cr "$R" 2>/dev/null || true
# Bundles non déplaçables (posés à leur chemin même si une copie existe ailleurs) et toujours remplacés.
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
pkgbuild --root "$R" --component-plist "$STAGE/component.plist" --scripts installer/scripts \
    --identifier fr.francois-brille.openlw --version "$VERSION" --install-location / \
    --ownership recommended "$STAGE/OpenLW-component.pkg"
sed "s/__VERSION__/$VERSION/" installer/distribution.xml >"$STAGE/distribution.xml"
OUT="build/OpenLW-$VERSION.pkg"
if [ -n "${INSTALLER_SIGN_ID:-}" ]; then
    productbuild --distribution "$STAGE/distribution.xml" --package-path "$STAGE" --resources installer/resources \
        --sign "$INSTALLER_SIGN_ID" "$OUT"
else
    productbuild --distribution "$STAGE/distribution.xml" --package-path "$STAGE" --resources installer/resources "$OUT"
fi
echo "OK : $ROOT/$OUT"
