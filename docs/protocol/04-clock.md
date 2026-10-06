# 04 — Horloge

## Fonctionnement d'OpenLW

OpenLW ne se synchronise pas encore sur une horloge réseau : l'audio est cadencé par l'horloge du Mac (48 kHz nominal). Avec les appareils testés, l'échange fonctionne sans horloge commune. L'écart entre les horloges est compensé par glissement dans les tampons de réception : une coupure très brève, rare, quand l'écart cumulé dépasse le tampon. Un asservissement (PTP, puis horloge Livewire) est prévu ([ADR 0003](../adr/0003-horloge.md)).

## Horloge Livewire

| Élément | Valeur | Mention |
|---|---|---|
| Transport | multicast **239.192.255.2**, UDP **7000** | Observé |
| Forme | paquet RTP avec extension d'en-tête, profil `0xFA1A`, longueur 20 mots (80 octets) | Hypothèse |
| Timestamp RTP | compteur d'échantillons à 48 kHz | Hypothèse |
| Cadence | un paquet toutes les 250 µs (timestamp +12) | Hypothèse |

Contenu supposé de la charge UDP (à confirmer en capture, [Q4](open-questions.md)) :

| Octets | Contenu |
|---|---|
| 0–11 | en-tête RTP |
| 12–13 | profil d'extension `FA 1A` |
| 14–15 | longueur d'extension `00 14` |
| 16–19 | numéro de séquence d'horloge |
| 20–23 | type de message : `0A 00 CA BA` (A) ou `0B 00 CA BA` (B) |
| 26–29 | identifiant du maître |
| autres | inconnus |

OpenLW décode ces paquets (`daemon/lw-proto/src/lwclock.rs`, `tools/lw/lwdump.py`) mais n'en émet pas. Vecteurs : [vectors/lwclock.json](vectors/lwclock.json).

## PTP (AES67)

PTPv2 (IEEE 1588-2008), profil média AES67 : groupe 224.0.1.129, ports 319 (événements) et 320 (général), domaine réglable (0 par défaut). Horloge média des flux AES67 (RFC 7273, `mediaclk:direct=0`) :

```
timestamp RTP = secondes × 48 000 + nanosecondes × 48 000 / 10⁹   (modulo 2³²)
```

OpenLW contient un décodeur PTP (`daemon/lw-proto/src/ptp.rs`) et un grandmaster logiciel de test (`tools/lw/ptp_gm.py`), pas encore d'esclave.
