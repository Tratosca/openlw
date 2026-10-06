#!/bin/sh
# Retire l'installation de développement (daemon, LaunchDaemon, plugin, app). À lancer avec sudo.
# La config et les journaux sont conservés, sauf avec --purge.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "à lancer avec sudo" >&2; exit 1; }
APP="/Library/Application Support/OpenLW"
launchctl bootout system/fr.francois-brille.openlw.daemon 2>/dev/null || true
rm -f /Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist
rm -f "$APP/lw-daemon"
rm -rf /Library/Audio/Plug-Ins/HAL/OpenLW.driver
rm -rf "/Applications/OpenLW.app"
if [ "${1:-}" = "--purge" ]; then
    rm -rf "$APP" /Library/Logs/OpenLW
fi
killall coreaudiod || true
echo "retiré"
