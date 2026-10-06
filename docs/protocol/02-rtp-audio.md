# 02 — Flux audio RTP

## Formats

Audio PCM linéaire entrelacé, 48 kHz. L24 big-endian (RFC 3190) par défaut ; L16 annoncé possible (`FAST = 3`, voir [03](03-advertisement.md)).

| Format | Échantillons par paquet | Intervalle | Paquets/s | Charge utile stéréo L24 | Mention |
|---|---|---|---|---|---|
| Standard | 240 | 5 ms | 200 | 1 440 octets | Observé |
| Livestream | 12 | 0,25 ms | 4 000 | 72 octets | Observé (taille), Choix OpenLW (émission) |
| AES67 | 48 | 1 ms | 1 000 | 288 octets | AES67 |
| Surround 8 canaux | 60 | 1,25 ms | 800 | 1 440 octets | Hypothèse |

La charge utile reste à 1 440 octets au plus : le surround garde la taille d'un paquet Standard.

## En-tête RTP

En-tête de 12 octets, sans CSRC ni extension (RFC 3550).

| Champ | Valeur | Mention |
|---|---|---|
| V, P, X, CC | 2, 0, 0, 0 (octet `0x80`) | Observé |
| M | 0 | Observé |
| PT | 96 par défaut (dynamique) | Observé |
| Séquence | +1 par paquet | Observé |
| Timestamp | +nombre d'échantillons par paquet (Δ = 240 en Standard) | Observé (Standard) |
| SSRC | les 4 octets de l'adresse du groupe de destination | Choix OpenLW |

Au démarrage d'un flux, OpenLW dérive la séquence et le timestamp de l'horloge du Mac (échantillons à 48 kHz depuis le démarrage du système). Un flux relancé reprend ainsi vers l'avant au lieu de repartir de zéro ; un récepteur y voit une perte, pas un retour en arrière. Choix OpenLW.

Réception : OpenLW suppose un en-tête de 12 octets et dérive le nombre d'échantillons de la longueur UDP. Un écart de séquence dans [−399, 0] est traité comme doublon ou retard ; un écart plus grand vers l'avant compte comme perte ; tout autre écart est une resynchronisation.

## IP, UDP, QoS

| Champ | Valeur | Mention |
|---|---|---|
| TTL multicast | 128 | Observé |
| DSCP | EF (46, octet TOS `0xB8`) par défaut ; AF41 (34) recommandé par AES67 ; réglable | Observé (EF) |
| Port source | égal au port de destination (5004) | Observé |
| Checksum UDP | calculé par la pile du Mac | Choix OpenLW |

Les sockets sont liées à l'interface Livewire (`IP_BOUND_IF`, `IP_MULTICAST_IF`). L'abonnement aux groupes passe par IGMP (pile macOS).

## SDP (AES67)

OpenLW produit et lit des descriptions SDP conformes à RFC 4566 et RFC 7273. Lignes produites, fins de ligne CRLF :

```
v=0
o=- <sess-id> <sess-version> IN IP4 <ip de l'hôte>
s=<nom de la source>
c=IN IP4 <groupe>
t=0 0
m=audio <port> RTP/AVP <pt>
a=rtpmap:<pt> L<bits>/<fréquence>/<canaux>
a=sendonly
a=ptime:<durée>
[a=ts-refclk:ptp=IEEE1588-2008:<identité du grandmaster>:<domaine>
 a=mediaclk:direct=0]
```

- `a=ptime` : millisecondes entières si le paquet en contient un nombre entier (`1`, `5`), sinon deux décimales (`1.25`, `0.25`).
- `ts-refclk` et `mediaclk` ne figurent que si un grandmaster PTP est connu.
- En lecture, `a=sync-time:<n>` (variante Ravenna) est accepté comme `a=mediaclk:direct=<n>`.

Vecteur : [vectors/sdp/aes67-ch101.sdp](vectors/sdp/aes67-ch101.sdp). Horloge média : [04-clock.md](04-clock.md).

Vecteurs d'en-têtes : [vectors/rtp_headers.json](vectors/rtp_headers.json).
