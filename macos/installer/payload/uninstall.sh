#!/bin/sh
# Uninstall OpenLW: network service, audio device, app, settings.
# Launched by OpenLW (Uninstall menu, administrator privileges) or:
#   sudo "/Library/Application Support/OpenLW/uninstall.sh"
set -u
[ "$(id -u)" -eq 0 ] || { echo "à lancer avec sudo" >&2; exit 1; }
launchctl bootout system/fr.francois-brille.openlw.daemon 2>/dev/null || true
rm -f /Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist
rm -rf /Library/Audio/Plug-Ins/HAL/OpenLW.driver
rm -rf "/Applications/OpenLW.app"
rm -rf /Library/Logs/OpenLW
pkgutil --forget fr.francois-brille.openlw >/dev/null 2>&1 || true
# coreaudiod unloads device (a few seconds of audio interruption).
killall coreaudiod 2>/dev/null || true
# Last: this directory contains this script.
rm -rf "/Library/Application Support/OpenLW"
echo "OpenLW désinstallé"
