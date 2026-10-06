# 03 — Annonce et découverte des sources

Les appareils Livewire annoncent leurs sources en multicast. OpenLW écoute ces annonces pour découvrir les sources du réseau, et annonce les canaux qu'il diffuse.

## Transport

| Élément | Valeur | Mention |
|---|---|---|
| Annonces | multicast **239.192.255.3**, UDP **4001** | Observé |
| Port de contrôle d'un terminal | UDP **4000** (annoncé dans `UDPC`), unicast | Observé |
| TTL | 128 | Observé |

Un datagramme = **enveloppe de 16 octets** + **message TLV**.

## Enveloppe (16 octets)

| Octet(s) | Valeur observée | Rôle |
|---|---|---|
| 0 | 3 | couche du message (message TLV) |
| 1 | 0 | datagramme sans acquittement |
| 2 | 2 | version du format TLV |
| 3 | 7 | version de l'enveloppe |
| 4–7 | numéro de séquence | incrémenté à chaque envoi, jamais 0 |
| 8–15 | 0 | non utilisés dans les annonces |

OpenLW rejette un datagramme dont l'octet 3 n'est pas 7.

## Message TLV

```
u32  identifiant du message (quatre caractères ASCII, ex. 'NEST')
u16  nombre d'entrées
entrées :
  u32  étiquette (quatre caractères ASCII, ex. 'PSNM')
  u8   type
  ...  valeur selon le type
```

| Type | Valeur |
|---|---|
| 1 | u32 |
| 2 | u16 longueur + octets |
| 3 | u16 longueur + chaîne (longueur fixe, complétée par des zéros) |
| 4 | u16 nombre + nombre × u16 |
| 5 | u16 nombre + nombre × u32 |
| 6 | u16 longueur + message TLV imbriqué (identifiant puis entrées) |
| 7 | u8 |
| 8 | u16 |
| 9 | u64 |

Un message imbriqué observé porte l'identifiant `INDI`, suivi du nombre d'entrées comme tout message.

## Annonce complète et annonce courte

```
'NEST'
  'PVER'  u16  2
  'ADVT'  u8   1 = annonce complète, 2 = annonce courte (keepalive)
  'TERM'  message 'INDI' : le terminal
     'ADVV'  u32  version de l'annonce (change quand la liste des sources change)
     'HWID'  u16  identifiant du terminal (16 bits bas de son adresse IP)
     'INIP'  u32  adresse IP du terminal
     'UDPC'  u16  port de contrôle (4000)
     'NUMS'  u16  nombre de sources annoncées
     'ATRN'  chaîne[32]  nom du terminal (annonce complète seulement)
  'S001' … 'S240'  message 'INDI' : une source par emplacement (annonce complète seulement)
     'PSID'  u32  canal Livewire
     'SHAB'  u8   source partageable
     'FSID'  u32  groupe du flux aller (239.192.x.y)
     'FAST'  u8   type du flux aller : 2 stéréo L24, 3 stéréo L16, 4 surround
     'FASM'  u8   mode du flux aller (1)
     'BSID'  u32  groupe de retour (239.193.x.y)
     'BAST'  u8   type du flux de retour
     'BASM'  u8   mode du flux de retour
     'LPID'  u32  canal (copie de PSID)
     'STPL'  u8   0
     'PSNM'  chaîne[16]  nom de la source
     'LABL'  chaîne[10]  libellé (facultatif)
```

Mentions : structure et étiquettes observées ; types des chaînes (3) observés ; sens de `SHAB`, `FASM`, `BAST`, `BASM`, `STPL` : hypothèse.

- Une annonce complète contient au plus **8 sources par datagramme** ; au-delà, elle est envoyée en pages successives avec la même `ADVV`.
- L'annonce courte a la même enveloppe `NEST`, avec `ADVT = 2`, un `TERM` sans `ATRN` et aucune source.
- Une étiquette de source est `S` suivi de trois chiffres ASCII (emplacement 1 à 240).
- Les noms sont en ASCII : OpenLW remplace les lettres accentuées (« François » → « Francois ») et complète par des zéros.

### Valeurs choisies par OpenLW

- `ADVV` : heure Unix (secondes) au démarrage de la session d'annonce. Elle change donc à chaque modification des sources annoncées, ce qui fait relire la liste aux autres appareils.
- `BAST = 0`, `BASM = 1` : OpenLW ne fournit pas de flux de retour.
- `SHAB = 0`, `FASM = 1`, `STPL = 0`.

## Cadence d'émission d'OpenLW

| Événement | Délai |
|---|---|
| Annonce complète | au démarrage, puis 1 s ± 0,5 s plus tard |
| Annonce courte | toutes les 20 s ± 5 s |
| Annonce complète périodique | après 8 annonces courtes |
| Pages d'une annonce complète | 100 ms ± 50 ms entre deux pages |

Observé sur un appareil Livewire : annonce courte environ toutes les 10 s.

## Découverte

- Un terminal est identifié par son adresse IP (`INIP`).
- Une annonce complète avec une nouvelle `ADVV` remplace la liste des sources du terminal ; les pages d'une même `ADVV` s'additionnent.
- Une annonce courte rafraîchit seulement le terminal.
- Un terminal muet depuis 75 s (trois annonces courtes manquées) est retiré.
- **Requête d'annonce complète** (Choix OpenLW) : quand une annonce courte arrive d'un terminal dont la liste n'est pas connue, OpenLW lui envoie en unicast, sur son port `UDPC`, au plus une fois toutes les 5 s :

```
enveloppe : 03 00 02 07 | séquence | 00 × 8
message 'READ', 1 entrée : 'ADVD' u8 = 1
```

La réponse des appareils à cette requête n'est pas établie ([open-questions.md](open-questions.md)). À défaut, la liste arrive avec la prochaine annonce complète périodique.

## Autre message observé

Certains appareils envoient aussi, environ toutes les 10 s, un message `ADVT = 3` : `TERM` réduit à `HWID`, puis une entrée `S001` avec `PSID`, une étiquette `BUSY` (u64) et deux entrées d'étiquettes `0xFFFFFFFF` et `0xFFFFFFFE` (u64). Sens : hypothèse (occupation de la source). OpenLW l'ignore.

Vecteurs : [vectors/adv_packets.json](vectors/adv_packets.json). Implémentations : `daemon/lw-proto/src/{envelope,tlv,adv}.rs`, `tools/lw/advcodec.py`. Dissecteur : `tools/wireshark/livewire.lua`.
