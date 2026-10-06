# 01 — Canal Livewire et groupe multicast

Un canal Livewire est un entier **N de 1 à 32 766**. Le canal occupe les 16 bits de poids faible de l'adresse multicast du flux ; le préfixe dépend du type de flux.

| Type de flux | Groupe | Mention |
|---|---|---|
| Stéréo (Standard, Livestream, AES67) | `239.192.(N >> 8).(N & 0xFF)` | Observé |
| Retour vers la source (backfeed) | `239.193.(N >> 8).(N & 0xFF)` | Observé dans les annonces (`BSID`) |
| Surround 8 canaux | `239.196.(N >> 8).(N & 0xFF)` | Hypothèse |

Inverse : `N = adresse & 0x7FFF`.

Exemples : canal 1 → `239.192.0.1` ; canal 101 → `239.192.0.101` ; canal 257 → `239.192.1.1` ; canal 32 766 → `239.192.127.254`.

Port audio : **UDP 5004** (observé).

Vecteurs : [vectors/chan2mcast.json](vectors/chan2mcast.json). Implémentations : `daemon/lw-proto/src/channel.rs`, `tools/lw/lwchan.py`.
