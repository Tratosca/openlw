# OpenLW installer

`macos/installer/build-pkg.sh <arch>` produces one package per architecture:

| Package | Macs | Minimum macOS |
|---|---|---|
| `build/OpenLW-<version>-arm64.pkg` | Apple Silicon | 11.0 |
| `build/OpenLW-<version>-x86_64.pkg` | Intel | 10.13 (embeds the Swift runtime for 10.13–10.14.3) |

Both are built on the same Mac: `build-all.sh` produces universal binaries, `build-pkg.sh` thins them to the requested architecture, checks that no other architecture remains, then signs. An installation check refuses a package that does not match the Mac: Core Audio loads the HAL plugin in the native architecture, so an Intel package would not work on Apple Silicon, even under Rosetta.

```sh
macos/scripts/build-all.sh
macos/installer/build-pkg.sh arm64
macos/installer/build-pkg.sh x86_64
# Signed: APP_SIGN_ID="Developer ID Application: …" INSTALLER_SIGN_ID="Developer ID Installer: …" macos/installer/build-pkg.sh arm64
```

## Signed and notarized release

`macos/scripts/release.sh` builds everything, signs the daemon, HAL plugin, app and embedded Swift runtime with the Developer ID Application identity (hardened runtime, secure timestamp, no entitlements), signs each package with the Developer ID Installer identity, notarizes it, staples the ticket and writes `build/OpenLW-<version>-<arch>.pkg.sha256`. `ARCHS=arm64` (or `x86_64`) limits the run to one architecture.

Requirements: both Developer ID certificates (Application and Installer) in the keychain, and an App Store Connect API key with the Developer role.

```sh
# Once: store the notarization key in the keychain.
xcrun notarytool store-credentials openlw-notary --key AuthKey_XXXXXXXXXX.p8 --key-id XXXXXXXXXX --issuer <issuer UUID>
NOTARY_PROFILE=openlw-notary macos/scripts/release.sh
# Signing only, nothing sent to Apple:
SKIP_NOTARIZE=1 macos/scripts/release.sh
```

On failure, `build/notary-<arch>.json` and `build/notary-log-<arch>.json` hold Apple's verdict and the per-file issues.

CI: `.github/workflows/release-macos.yml` runs the same script on a `vX.Y.Z` tag (it must match `macos/app/Info.plist`) or by hand, in the `release` GitHub environment. Certificates are imported into a temporary keychain deleted at the end of the job. The signed packages are kept as a run artifact; nothing is published. Secrets to define in that environment:

| Secret | Content |
|---|---|
| `MACOS_APP_CERT_P12` | base64 of the Developer ID Application `.p12` (certificate and private key) |
| `MACOS_INSTALLER_CERT_P12` | base64 of the Developer ID Installer `.p12` |
| `MACOS_CERT_PASSWORD` | password of both `.p12` files |
| `NOTARY_KEY_P8` | base64 of the App Store Connect API key (`.p8`) |
| `NOTARY_KEY_ID` | key ID |
| `NOTARY_ISSUER_ID` | issuer ID |

`base64 -i file.p12 | pbcopy` copies a value. Add required reviewers to the environment so that every signing run is approved.

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

- Release workflow on GitHub Actions: not run yet (no remote). The local chain is validated: 0.5.0 arm64 and x86_64 packages notarized by Apple without any issue, stapled, accepted by Gatekeeper. The installation check was tested outside Installer only.
- Validate actual installation on this Mac, then on 10.13 (untested). Check that package `._*` entries (`com.apple.provenance` attribute) leave no files on disk.
