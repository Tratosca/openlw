# OpenLW

**English summary.** OpenLW is an open-source macOS audio driver for Livewire®-compatible and AES67 audio-over-IP networks. It adds a virtual CoreAudio device to the Mac: any application can record network channels from it and play audio that OpenLW sends to the network on the channels you choose. It discovers the sources announced on the network, lets you patch them to the Mac's inputs, and announces the Mac's outputs. macOS 10.13 and later, Intel and Apple Silicon. Licensed under Apache-2.0. Not affiliated with or endorsed by TLS Corp. (Telos Alliance). The documentation below is in French.

---

OpenLW est un driver audio libre pour macOS, compatible avec les réseaux Livewire® et AES67.

- **Périphérique audio** : « OpenLW » (ou « OpenLW In » et « OpenLW Out »), utilisable par toutes les applications du Mac, de 1 à 16 canaux stéréo dans chaque sens.
- **Réception** : découverte des sources annoncées sur le réseau, patch d'un canal Livewire vers une paire d'entrées, pré-écoute au casque.
- **Diffusion** : chaque paire de sorties du Mac part sur le canal Livewire choisi, sous le nom choisi, et est annoncée aux autres appareils.
- **Réseau** : interface choisie automatiquement (celle qui entend des annonces Livewire), modifications appliquées sans coupure des autres flux.
- **App « OpenLW »** : patch, diffusion, vumètres, réglages, désinstallation.

## Installation

Téléchargez `OpenLW-<version>.pkg`, ouvrez-le et suivez l'installeur. Ensuite :

1. Reliez le Mac au réseau Livewire.
2. Ouvrez **OpenLW** (dossier Applications).
3. Pour diffuser : choisissez « OpenLW » comme sortie du Mac ou de l'application, saisissez le canal, cochez **Diffuser**.
4. Pour enregistrer : cliquez la case de la source dans la grille des entrées, puis enregistrez depuis « OpenLW ».

Désinstallation : menu OpenLW > Désinstaller OpenLW.

Le paquet n'est pas encore signé ni notarisé : macOS demande une confirmation à l'ouverture (clic droit > Ouvrir, ou Réglages Système > Confidentialité et sécurité).

## Construire depuis les sources

Prérequis : Xcode (SDK macOS 26 ou plus), Rust 1.82 ou plus avec les cibles `x86_64-apple-darwin` et `aarch64-apple-darwin`, Python 3 pour les outils et les tests.

```sh
macos/scripts/build-all.sh          # service, plugin et app (binaires universels)
macos/installer/build-pkg.sh        # build/OpenLW-<version>.pkg
```

Tests :

```sh
(cd daemon && cargo test && cargo clippy --all-targets)
make -C macos/plugin test
python3 -m pytest -q tools/lw/tests
```

## Organisation

| Dossier | Contenu |
|---|---|
| `macos/plugin/` | plugin CoreAudio (AudioServerPlugIn, C) : le périphérique audio |
| `daemon/` | service réseau (Rust) : RTP, annonces, découverte, patch, contrôle XPC |
| `macos/app/` | app OpenLW (Swift, AppKit) |
| `macos/installer/` | paquet `.pkg` |
| `docs/protocol/` | spécification réseau (ce qu'OpenLW émet et accepte) |
| `docs/adr/` | décisions d'architecture |
| `tools/lw/` | outils Python : codecs, émetteurs de test, analyse de captures |
| `tools/wireshark/` | dissecteur Wireshark |

## Limites

- Pas encore d'asservissement à une horloge réseau (PTP ou horloge Livewire) : l'écart d'horloge est compensé par glissement de tampon, avec une coupure brève et rare ([ADR 0003](docs/adr/0003-horloge.md)).
- Surround et AES67 non vérifiés avec des appareils tiers ([points ouverts](docs/protocol/open-questions.md)).
- Non testé sur macOS 10.13.

## Licence et marques

Licence Apache, version 2.0 : voir [LICENSE](LICENSE) et [NOTICE](NOTICE).

Livewire, Livewire+ et Axia sont des marques de TLS Corp. (Telos Alliance). OpenLW est un projet indépendant, ni affilié à TLS Corp., ni approuvé par elle ; ces noms n'indiquent que la compatibilité. OpenLW ne contient aucun code, binaire ou document de TLS Corp.

Copyright 2026 François Brille (Tratosca).
