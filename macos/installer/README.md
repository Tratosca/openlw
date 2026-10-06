# OpenLW installer

`macos/installer/build-pkg.sh` produces `build/OpenLW-<version>.pkg`: one package for Intel and Apple Silicon, macOS 10.13 and later.

```sh
macos/scripts/build-all.sh
macos/installer/build-pkg.sh
# Signed: INSTALLER_SIGN_ID="Developer ID Installer: …" macos/installer/build-pkg.sh
```

## Installed files

| Path | Purpose |
|---|---|
| `/Library/Audio/Plug-Ins/HAL/OpenLW.driver` | “OpenLW” audio device |
| `/Library/Application Support/OpenLW/lw-daemon` | Network service |
| `/Library/Application Support/OpenLW/lw-daemon.json` | Settings, created from `lw-daemon.default.json` if absent |
| `/Library/Application Support/OpenLW/uninstall.sh` | Uninstaller (OpenLW menu) |
| `/Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist` | Service startup at boot |
| `/Applications/OpenLW.app` | Configuration app |
| `/Library/Logs/OpenLW/` | Service error log |

Package identifier: `fr.francois-brille.openlw`. Bundles are non-relocatable and always replaced.

## Scripts

- `preinstall`: stops the existing service. Also removes development versions predating the OpenLW name.
- `postinstall`: creates default settings if absent, starts the service, and restarts `coreaudiod` to load the device (a few seconds of audio interruption).

Defaults: automatic interface selection, output advertisements enabled, two channels per direction, advertised name = computer name.

## Verify a package

```sh
pkgutil --payload-files build/OpenLW-*.pkg
pkgutil --expand build/OpenLW-*.pkg /tmp/lwpkg && cat /tmp/lwpkg/Distribution
```

## Pending work

- Developer ID Application signing of the app, plugin, and service (hardened runtime), followed by notarization (`xcrun notarytool submit … --wait`, `xcrun stapler staple`).
- Validate actual installation on this Mac, then on 10.13 (untested). Check that package `._*` entries (`com.apple.provenance` attribute) leave no files on disk.
