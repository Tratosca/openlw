#!/bin/sh
# Installation de développement : daemon (LaunchDaemon), plugin HAL et app. À lancer avec sudo
# après scripts/build-all.sh. Retrait : sudo scripts/uninstall-dev.sh
#
# Fichiers installés :
#   /Library/Application Support/OpenLW/lw-daemon        (root:wheel 755)
#   /Library/Application Support/OpenLW/lw-daemon.json   (conservé s'il existe déjà)
#   /Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist        (root:wheel 644)
#   /Library/Logs/OpenLW/                                         (journal stderr du daemon)
#   /Library/Audio/Plug-Ins/HAL/OpenLW.driver             (root:wheel)
#   /Applications/OpenLW.app
set -eu
[ "$(id -u)" -eq 0 ] || { echo "à lancer avec sudo" >&2; exit 1; }
cd "$(dirname "$0")/.."
APP="/Library/Application Support/OpenLW"
PLIST=/Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist
LABEL=fr.francois-brille.openlw.daemon
HAL=/Library/Audio/Plug-Ins/HAL
[ -x build/lw-daemon ] && [ -d plugin/build/OpenLW.driver ] || { echo "lancer d'abord scripts/build-all.sh" >&2; exit 1; }

echo "1/5 daemon"
launchctl bootout system/$LABEL 2>/dev/null || true
mkdir -p "$APP" /Library/Logs/OpenLW
install -o root -g wheel -m 755 build/lw-daemon "$APP/lw-daemon"
if [ ! -f "$APP/lw-daemon.json" ]; then
    install -o root -g wheel -m 644 scripts/lw-daemon.dev.json "$APP/lw-daemon.json"
else
    echo "   config existante conservée : $APP/lw-daemon.json"
fi

echo "2/5 LaunchDaemon"
install -o root -g wheel -m 644 daemon/launchd/fr.francois-brille.openlw.daemon.plist "$PLIST"
launchctl bootstrap system "$PLIST"

echo "3/5 plugin HAL"
rm -rf "$HAL/OpenLW.driver"
cp -R plugin/build/OpenLW.driver "$HAL/"
chown -R root:wheel "$HAL/OpenLW.driver"

echo "4/5 redémarrage de coreaudiod (coupure audio de quelques secondes)"
killall coreaudiod || true
sleep 3

echo "5/5 app OpenLW"
if [ -d "app/build/OpenLW.app" ]; then
    rm -rf "/Applications/OpenLW.app"
    cp -R "app/build/OpenLW.app" /Applications/
else
    echo "   app absente (make -C app) : étape ignorée"
fi

echo
echo "Vérifications :"
echo "  build/lw-daemon ctl status          # daemon joignable, état du périphérique"
echo "  open '/Applications/OpenLW.app'"
echo "  log stream --info --predicate 'subsystem == \"fr.francois-brille.openlw\"'"
