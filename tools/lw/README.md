# tools/lw — outils Livewire en Python

Python 3 (bibliothèque standard uniquement ; `pytest` pour les tests). Les commandes supposent la racine du dépôt comme répertoire courant. Spécification : [docs/protocol/](../../docs/protocol/README.md).

| Outil | Rôle |
|---|---|
| `lwchan.py` | canal ↔ groupe multicast (stéréo, backfeed, surround) |
| `advcodec.py` | codec Envelope + TlvMsg, construction et décodage des annonces |
| `pcapio.py` | lecture pcap/pcapng, décodage Ethernet/802.1Q/IPv4/UDP, écriture pcap synthétique |
| `rtpstats.py` | statistiques par flux RTP (PT, SSRC, taille, Δseq, Δts, gigue, TOS, TTL) |
| `lwdump.py` | résumé ADV, horloge Livewire (7000), PTP (319/320) d'une capture |
| `sdptool.py` | génération et vérification de SDP AES67 |
| `netiface.py` | sockets UDP liées à une interface (`IP_BOUND_IF` / `SO_BINDTODEVICE`, `IP_MULTICAST_IF`) |
| `mcast_join.py` | garde des groupes ouverts pendant une capture (IGMP snooping) |
| `emit_rtp.py` | émetteur RTP de test (standard, aes67, livestream, surround) |
| `emit_adv.py` | annonce d'une source factice |
| `ptp_gm.py` | grandmaster PTPv2 logiciel de test (Sync/Follow_Up/Announce, Delay_Resp) ; pas de BMCA |
| `make_vectors.py` | régénère `docs/protocol/vectors/` |

Tests :

```sh
python3 -m pytest -q tools/lw/tests
```

Les émetteurs (`emit_*`) envoient du trafic multicast réel sur l'interface donnée : à utiliser uniquement sur un réseau de test, jamais sur un réseau Livewire en exploitation (un canal en double coupe la source légitime).
