# Installeur OpenLW

`macos/installer/build-pkg.sh` produit `build/OpenLW-<version>.pkg` : un paquet unique, Intel et Apple Silicon, macOS 10.13 et plus.

```sh
macos/scripts/build-all.sh
macos/installer/build-pkg.sh
# signé : INSTALLER_SIGN_ID="Developer ID Installer: …" macos/installer/build-pkg.sh
```

## Fichiers posés

| Chemin | Rôle |
|---|---|
| `/Library/Audio/Plug-Ins/HAL/OpenLW.driver` | périphérique audio « OpenLW » |
| `/Library/Application Support/OpenLW/lw-daemon` | service réseau |
| `/Library/Application Support/OpenLW/lw-daemon.json` | réglages, créés depuis `lw-daemon.default.json` s'ils n'existent pas |
| `/Library/Application Support/OpenLW/uninstall.sh` | désinstalleur (menu de OpenLW) |
| `/Library/LaunchDaemons/fr.francois-brille.openlw.daemon.plist` | démarrage du service au boot |
| `/Applications/OpenLW.app` | app de réglage |
| `/Library/Logs/OpenLW/` | journal d'erreurs du service |

Identifiant du paquet : `fr.francois-brille.openlw`. Bundles non déplaçables et toujours remplacés.

## Scripts

- `preinstall` : arrête le service en place. Retire aussi les versions de développement antérieures au nom OpenLW.
- `postinstall` : crée les réglages par défaut s'ils manquent, démarre le service, redémarre `coreaudiod` pour charger le périphérique (coupure du son de quelques secondes).

Réglages par défaut : interface automatique, annonce des sorties activée, 2 canaux dans chaque sens, nom annoncé = nom de l'ordinateur.

## Vérifier un paquet

```sh
pkgutil --payload-files build/OpenLW-*.pkg
pkgutil --expand build/OpenLW-*.pkg /tmp/lwpkg && cat /tmp/lwpkg/Distribution
```

## Reste à faire

- Signature Developer ID Application de l'app, du plugin et du service (runtime renforcé), puis notarisation (`xcrun notarytool submit … --wait`, `xcrun stapler staple`).
- Installation réelle à valider sur ce Mac, puis sur 10.13 (non testé). Vérifier que les entrées `._*` du paquet (attribut `com.apple.provenance`) ne laissent aucun fichier sur le disque.
