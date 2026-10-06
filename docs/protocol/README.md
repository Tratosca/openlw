# Spécification réseau d'OpenLW

Ce dossier décrit ce qu'OpenLW émet et accepte sur un réseau Livewire® / AES67 : adressage des canaux, flux audio RTP, annonce des sources, horloge. C'est la référence des tests (`vectors/`) et des outils (`tools/lw/`, `tools/wireshark/`).

Les informations viennent de l'observation du trafic d'appareils compatibles (captures réseau) et des normes publiques : RFC 3550 (RTP), RFC 3190 (L24), RFC 4566 (SDP), RFC 7273 (horloge média), AES67, IEEE 1588 (PTP).

| Document | Sujet |
|---|---|
| [01-channels.md](01-channels.md) | Canal Livewire et groupe multicast |
| [02-rtp-audio.md](02-rtp-audio.md) | Flux audio : formats, en-têtes RTP, SDP, QoS |
| [03-advertisement.md](03-advertisement.md) | Annonce et découverte des sources |
| [04-clock.md](04-clock.md) | Horloge Livewire et PTP |
| [open-questions.md](open-questions.md) | Points non établis |

## Conventions

Chaque affirmation porte, si besoin, l'une de ces mentions :

- **Observé** : vu dans des captures du trafic d'appareils Livewire.
- **Choix OpenLW** : comportement d'OpenLW, compatible avec les appareils observés, sans être imposé par eux.
- **Hypothèse** : supposé, non vérifié sur le réseau ; listé dans [open-questions.md](open-questions.md).

Octets en ordre réseau (big-endian), sauf mention contraire.

## Marques

Livewire, Livewire+ et Axia sont des marques de TLS Corp. (Telos Alliance). OpenLW n'est ni affilié à TLS Corp., ni approuvé par elle. Ces noms désignent ici le protocole avec lequel OpenLW est compatible.
