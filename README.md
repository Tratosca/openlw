# OpenLW

**English summary.** OpenLW is an open-source audio driver for Livewire®-compatible and AES67 audio-over-IP networks. On macOS (10.13 and later, Intel and Apple Silicon) it adds a virtual CoreAudio device: any application can record network channels from it and play audio that OpenLW sends to the network on the channels you choose. It discovers the sources announced on the network, lets you patch them to the computer's inputs, and announces its outputs. Windows (ASIO, x64 and ARM64) and Linux (PipeWire, x86_64 and ARM64) support is in progress: the network service already runs on all three systems. Licensed under Apache-2.0. Not affiliated with or endorsed by TLS Corp. (Telos Alliance). The documentation below is in French.

---

OpenLW est un driver audio libre, compatible avec les réseaux Livewire® et AES67.

| Système | Architectures | Périphérique audio | App | État |
|---|---|---|---|---|
| macOS 10.13 et plus | Intel, Apple Silicon | CoreAudio (toutes les applications) | OpenLW (AppKit, Liquid Glass sous macOS 26) | disponible |
| Windows 10 22H2 et 11 | x64, ARM64 | ASIO (applications compatibles ASIO) | OpenLW (WinUI 3) | en cours : service réseau prêt |
| Linux (PipeWire) | x86_64, ARM64 | nœuds PipeWire (applications PipeWire, PulseAudio, JACK) | OpenLW (GTK4) | en cours : service réseau prêt |

Feuille de route : [docs/roadmap.md](docs/roadmap.md). Ce qui suit décrit la version macOS.

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

Service réseau (tous systèmes) : Rust 1.82 ou plus ; un compilateur C (Clang, GCC ou MSVC).

```sh
(cd daemon && cargo build --release)    # daemon/target/release/lw-daemon
```

macOS : Xcode (SDK macOS 26 ou plus), cibles Rust `x86_64-apple-darwin` et `aarch64-apple-darwin`.

```sh
macos/scripts/build-all.sh          # service, plugin et app (binaires universels)
macos/installer/build-pkg.sh        # build/OpenLW-<version>.pkg
```

Tests :

```sh
(cd daemon && cargo test && cargo clippy --all-targets)
make -C macos/plugin test
python3 -m pytest -q tools/lw/tests
tools/ci/test-wine.sh               # daemon compilé pour Windows, sous Wine (Docker)
```

La CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)) teste le daemon sur macOS, Linux et Windows, en x86_64 et ARM64.

## Organisation

| Dossier | Contenu |
|---|---|
| `daemon/` | service réseau (Rust, tous systèmes) : RTP, annonces, découverte, patch, canal de contrôle |
| `macos/plugin/` | plugin CoreAudio (AudioServerPlugIn, C) : le périphérique audio |
| `macos/app/` | app OpenLW (Swift, AppKit) |
| `macos/installer/` | paquet `.pkg` |
| `windows/` | service, pilote ASIO, app et installeur Windows (en cours) |
| `linux/` | unité systemd, app et paquets Linux (en cours) |
| `docs/protocol/` | spécification réseau (ce qu'OpenLW émet et accepte) |
| `docs/adr/` | décisions d'architecture |
| `tools/lw/` | outils Python : codecs, émetteurs de test, analyse de captures |
| `tools/wireshark/` | dissecteur Wireshark |
| `tools/ci/` | tests du daemon Windows sous Wine (Docker) |

## Limites

- Pas encore d'asservissement à une horloge réseau (PTP ou horloge Livewire) : l'écart d'horloge est compensé par glissement de tampon, avec une coupure brève et rare ([ADR 0003](docs/adr/0003-horloge.md)).
- Surround et AES67 non vérifiés avec des appareils tiers ([points ouverts](docs/protocol/open-questions.md)).
- Non testé sur macOS 10.13.

## Licence et marques

Licence Apache, version 2.0 : voir [LICENSE](LICENSE) et [NOTICE](NOTICE).

Livewire, Livewire+ et Axia sont des marques de TLS Corp. (Telos Alliance). OpenLW est un projet indépendant, ni affilié à TLS Corp., ni approuvé par elle ; ces noms n'indiquent que la compatibilité. OpenLW ne contient aucun code, binaire ou document de TLS Corp.

Copyright 2026 François Brille (Tratosca).
