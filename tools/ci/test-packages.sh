#!/bin/sh
# Install the Linux packages (linux/packaging/dist/) on clean distribution images:
# - Debian 12: openlw-daemon only (libadwaita 1.2: no app);
# - Ubuntu 24.04: openlw-daemon and openlw;
# - Fedora 43: both RPMs.
# Checks dependencies resolution, installed files, user unit enabled for all users, binaries
# start, and clean removal.
#   tools/ci/test-packages.sh            PLATFORM=linux/amd64 for x86_64
set -eu
cd "$(dirname "$0")/../.."
PLATFORM=${PLATFORM:-linux/arm64}
if [ "$PLATFORM" = linux/amd64 ]; then ARCH=amd64 RPMARCH=x86_64; else ARCH=arm64 RPMARCH=aarch64; fi
DIST=linux/packaging/dist

# t MESSAGE COMMAND...: fails the run when COMMAND fails.
lib='t() { m=$1; shift; if "$@" >/dev/null 2>&1; then echo "ok   $m"; else echo "FAIL $m"; exit 1; fi; }'
check='
t "unit installed" test -f /usr/lib/systemd/user/openlw.service
t "user unit enabled for all users" test -L /etc/systemd/user/default.target.wants/openlw.service
t "lw-daemon starts" lw-daemon --help
'
check_app='
t "desktop entry" test -f /usr/share/applications/fr.francois_brille.openlw.desktop
t "AppStream metadata" test -f /usr/share/metainfo/fr.francois_brille.openlw.metainfo.xml
t "icon" test -f /usr/share/icons/hicolor/scalable/apps/fr.francois_brille.openlw.svg
t "openlw starts (--help)" openlw --help
'
removed='
t "unit disabled on removal" test ! -e /etc/systemd/user/default.target.wants/openlw.service
t "files removed" test ! -e /usr/bin/lw-daemon
'

echo "=== Debian 12: openlw-daemon"
docker run --rm --platform "$PLATFORM" -v "$PWD/$DIST":/dist:ro debian:12 sh -ec "
$lib
apt-get update -qq >/dev/null
apt-get install -y -qq systemd /dist/openlw-daemon_*_$ARCH.deb >/dev/null
$check
apt-get remove -y -qq openlw-daemon >/dev/null
$removed"

echo "=== Ubuntu 24.04: openlw-daemon, openlw"
docker run --rm --platform "$PLATFORM" -v "$PWD/$DIST":/dist:ro ubuntu:24.04 sh -ec "
$lib
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd /dist/openlw-daemon_*_$ARCH.deb /dist/openlw_*_$ARCH.deb >/dev/null
$check
$check_app
apt-get remove -y -qq openlw openlw-daemon >/dev/null
$removed"

if ls "$DIST"/openlw-daemon-*."$RPMARCH".rpm >/dev/null 2>&1; then
    echo "=== Fedora 43: openlw-daemon, openlw"
    docker run --rm --platform "$PLATFORM" -v "$PWD/$DIST":/dist:ro fedora:43 sh -ec "
$lib
dnf install -y -q systemd /dist/openlw-daemon-*.$RPMARCH.rpm /dist/openlw-[0-9]*.$RPMARCH.rpm >/dev/null
$check
$check_app
dnf remove -y -q openlw openlw-daemon >/dev/null
$removed"
fi
echo SUCCESS
