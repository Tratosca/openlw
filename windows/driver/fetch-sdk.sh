#!/bin/sh
# Télécharge le SDK ASIO de Steinberg (double licence ; utilisé ici sous GPLv3) dans sdk/,
# après vérification de son empreinte. Le SDK n'est jamais versionné dans le dépôt.
#   windows/driver/fetch-sdk.sh
set -eu
cd "$(dirname "$0")"
URL="https://download.steinberg.net/sdk_downloads/ASIO-SDK_2.3.4_2025-10-15.zip"
SHA256="d5ebf0c20dd2c5f43771fd0c1418f4b361bf52434ee670097cfa6b3a335e2eca"
[ -f sdk/ASIOSDK/common/iasiodrv.h ] && exit 0
mkdir -p sdk
curl -fsSL "$URL" -o sdk/asiosdk.zip
echo "$SHA256  sdk/asiosdk.zip" | sha256sum -c - 2>/dev/null || echo "$SHA256  sdk/asiosdk.zip" | shasum -a 256 -c -
(cd sdk && unzip -q -o asiosdk.zip && rm asiosdk.zip)
echo "SDK ASIO : $(pwd)/sdk/ASIOSDK"
