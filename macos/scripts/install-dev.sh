#!/bin/sh
# Development installation: daemon (LaunchDaemon), HAL plugin, app. Run with sudo
# after macos/scripts/build-all.sh. Remove: sudo macos/scripts/uninstall-dev.sh
#
# Installed files:
#   /Library/Application Support/OpenLW/lw-daemon        (root:wheel 755)
#   /Library/Application Support/OpenLW/lw-daemon.json   (preserved if already present)
#   /Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist        (root:wheel 644)
#   /Library/Logs/OpenLW/                                         (daemon stderr log)
#   /Library/Audio/Plug-Ins/HAL/OpenLW.driver             (root:wheel)
#   /Applications/OpenLW.app
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
cd "$(dirname "$0")/../.."
APP="/Library/Application Support/OpenLW"
PLIST=/Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist
LABEL=fr.francois-brille.openlw.daemon
HAL=/Library/Audio/Plug-Ins/HAL
[ -x build/lw-daemon ] && [ -d macos/plugin/build/OpenLW.driver ] || { echo "run macos/scripts/build-all.sh first" >&2; exit 1; }

echo "1/5 daemon"
launchctl bootout system/$LABEL 2>/dev/null || true
mkdir -p "$APP" /Library/Logs/OpenLW
install -o root -g wheel -m 755 build/lw-daemon "$APP/lw-daemon"
if [ ! -f "$APP/lw-daemon.json" ]; then
    install -o root -g wheel -m 644 macos/scripts/lw-daemon.dev.json "$APP/lw-daemon.json"
else
    echo "   existing config kept: $APP/lw-daemon.json"
fi

echo "2/5 LaunchDaemon"
install -o root -g wheel -m 644 macos/launchd/fr.francois-brille.openlw.daemon.plist "$PLIST"
launchctl bootstrap system "$PLIST"

echo "3/5 HAL plugin"
rm -rf "$HAL/OpenLW.driver"
cp -R macos/plugin/build/OpenLW.driver "$HAL/"
chown -R root:wheel "$HAL/OpenLW.driver"

echo "4/5 restarting coreaudiod (audio interrupted for a few seconds)"
killall coreaudiod || true
sleep 3

echo "5/5 OpenLW app"
if [ -d "macos/app/build/OpenLW.app" ]; then
    rm -rf "/Applications/OpenLW.app"
    cp -R "macos/app/build/OpenLW.app" /Applications/
else
    echo "   app missing (make -C macos/app): step skipped"
fi

echo
echo "Checks:"
echo "  build/lw-daemon ctl status          # daemon reachable, device state"
echo "  open '/Applications/OpenLW.app'"
echo "  log stream --info --predicate 'subsystem == \"fr.francois-brille.openlw\"'"
