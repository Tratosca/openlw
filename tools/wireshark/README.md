# Dissecteurs Livewire pour Wireshark

`livewire.lua` décode Envelope/TlvMsg (`lwadv`), l'horloge (`lwclock`) et le RTP
Livewire (`lwrtp`), d'après la [spécification](../../docs/protocol/README.md).

## Installation

Prérequis : Wireshark/TShark avec Lua ; Python 3 pour le générateur.
Validé avec TShark 4.6.2 et Lua 5.4.7 sur macOS.

Copier `livewire.lua` dans le dossier **plugins personnel Lua** indiqué par
Wireshark dans **À propos de Wireshark → Dossiers**, puis relancer Wireshark.
Autre possibilité : charger explicitement le script, depuis la racine du dépôt :

```sh
/Applications/Wireshark.app/Contents/MacOS/Wireshark \
  -X lua_script:tools/wireshark/livewire.lua

/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap -V
```

Ne pas cumuler installation personnelle et chargement `-X` du même script.
Les appels utilisent les [API Lua officielles Wireshark](https://www.wireshark.org/docs/wsdg_html_chunked/lua_module_Proto.html).

## Ports et préférences

Dans les préférences des protocoles Livewire, ou avec `tshark -o nom:valeur` :

| Préférence | Défaut | Usage |
|---|---:|---|
| `lwadv.control_port` | 4000 | Requête / contrôle Envelope |
| `lwadv.announce_port` | 4001 | Annonce Envelope |
| `lwclock.port` | 7000 | Horloge RTP |
| `lwrtp.port` | 5004 | Audio RTP |

Exemple : `-o lwrtp.port:15004`. La valeur 0 désactive une liaison ; les valeurs
supérieures à 65535 sont ignorées. Utiliser des ports distincts entre protocoles.
`lwrtp` est un dissecteur UDP dédié, et prend la place du décodage RTP habituel
sur son port ; il expose ses propres champs, sans créer de champs `rtp.*`.

## Filtres utiles

```text
lwadv
lwadv.advt == 1
lwadv.message_id == "READ"
lwadv.tag == "PSNM"
lwadv.channel == 101
lwadv.ip == 239.192.0.101
lwclock
lwclock.master_id == 12:34:56:78
lwrtp.channel == 101
lwrtp.kind == "surround"
lwrtp.samples == 240
lwrtp.ssrc_matches_dst == false
_ws.malformed || _ws.expert.severity == error
```

Les tags FourCC sont affichés en ASCII ; les octets non imprimables sont échappés
`\xNN`. Les types TlvMsg 1 à 9 sont décodés ; les tableaux restent typés et les
u64 gardent leur précision. Les types 2/3 affichent les octets et une chaîne si
son contenu est ASCII imprimable, après retrait des NUL terminaux éventuels.
Les champs PSID/LPID exposent le canal masqué sur 15 bits ; FSID/BSID/INIP
exposent aussi une adresse IPv4.

Le résumé ADV affiche le nom et les sources présents dans le paquet : aucune
mémorisation d'annonce antérieure. Une annonce courte affiche donc `<absent>`
pour ATRN et `<aucune>` pour les sources. Un paquet mal formé peut avoir un
résumé partiel ; consulter les informations expert.

## Génération et validation

Le générateur utilise uniquement la bibliothèque standard Python. Il écrit un
pcap classique Ethernet, sans émission réseau. Le chemin donné est écrasé.
Conserver les captures hors du dépôt :

```sh
python3 tools/wireshark/make_sample_pcap.py /tmp/lw_sample.pcap
/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap -V
/Applications/Wireshark.app/Contents/MacOS/tshark \
  -X lua_script:tools/wireshark/livewire.lua -r /tmp/lw_sample.pcap \
  -Y "_ws.malformed || _ws.expert.severity == error"
```

Résultat attendu : huit paquets, uniquement le **paquet 8** dans le filtre
expert, aucune erreur Lua.

| Paquet | Contenu attendu |
|---|---|
| 1 | ADV full, LW-SAMPLE, NUMS=1, S001, canal 101 |
| 2 | ADV short, NUMS=1, sans ATRN ni source |
| 3–5 | RTP PT 96, stéréo, canal 101, 240 échantillons, séquences 100–102 |
| 6 | RTP PT 96, surround, canal 5, 60 échantillons sur 8 canaux |
| 7 | Horloge : 44 octets de charge UDP au total, identité `12345678` |
| 8 | ADV volontairement tronqué dans le sous-message S001 |

## Limites

- Le format complet de l'horloge reste une **hypothèse**. Les offsets 26 à 29
  sont comptés depuis le début de la charge UDP, à partir de zéro. Le reste de
  la charge RTP est brut ; PT, SSRC et données synthétiques du générateur ne
  constituent pas une preuve de format matériel.
- Les annotations audio supposent **L24** : taille de charge RTP divisée par
  `3 × nombre de canaux`, avec 8 canaux pour `239.196/16`, sinon 2 pour
  `239.192/16` et `239.193/16`. Le PT dynamique ne prouve pas l'encodage.
  L16 et les sessions SDP ne sont pas identifiés automatiquement. Les CSRC,
  extensions et octets de bourrage RTP sont exclus du calcul de charge.
- TlvMsg est limité à **8 niveaux, racine comprise**. Troncature, dépassement
  d'une sous-longueur, type inconnu et profondeur excessive produisent une
  information expert `Malformed/Error`. Les ACK/NACK Envelope sans TlvMsg sont acceptés.
- Pas de reconstitution des annonces multipages ni de remplacement du dissecteur PTP
  natif. Tout autre message reçu sur un port d'annonce reste lisible comme TlvMsg générique.
- Les tests sont synthétiques, construits d'après la spécification ; ils
  ne valident pas l'interopérabilité avec un équipement Livewire réel.
