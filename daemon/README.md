# daemon — pile Livewire / AES67 en Rust

Workspace Cargo du service réseau d'OpenLW ([ADR 0001](../docs/adr/0001-daemon-rust.md)), pour macOS, Linux et Windows en x86_64 et ARM64 ([ADR 0006](../docs/adr/0006-multiplateforme.md)).

| Crate | Rôle | État |
|---|---|---|
| `lw-proto` | Codecs purs, sans I/O : canaux ↔ groupes, formats de flux, RTP et L24, TlvMsg, Envelope, annonce (ADV), SDP, PTPv2, horloge Livewire (décodage) | fait |
| `lw-sys` | Couche système, une version C par système (`csrc/macos`, `csrc/linux`, `csrc/windows`, `csrc/posix`) : threads temps réel (Mach, `SCHED_FIFO`, MMCSS), sommeil précis, horloge hôte, journal, **région partagée avec le client audio** (`csrc/lw_shm.h`/`.c` v2 : anneaux SPSC, seqlock d'horloge), canal de contrôle (`ctl` : XPC, socket Unix, tube nommé), DSCP qWAVE et service Windows. Seule crate avec `unsafe`, chaque bloc justifié (`SAFETY:`) | fait |
| `lw-daemon` | Sockets liées à la NIC (`IP_BOUND_IF`, `SO_BINDTODEVICE`, `IP_MULTICAST_IF`), émission RTP temps réel (générateur de test), réception avec statistiques, annonce ADV, config JSON, canal de contrôle (`status`, `ping`, `attach`, patch), périphérique virtuel (horloge, crêtes, boucle interne) | fait ; PTP esclave à faire |

Chaque module de `lw-proto` renvoie à sa fiche de [docs/protocol/](../docs/protocol/README.md). Les décodeurs traitent du trafic non authentifié : ils renvoient une erreur sur toute entrée invalide, sans paniquer. `unsafe` est interdit dans le workspace, et le lint `clippy::indexing_slicing` signale tout accès indexé hors tests.

## Commandes (depuis `daemon/`)

```sh
cargo test                                   # tests unitaires, vecteurs, robustesse
cargo clippy --all-targets                   # 0 avertissement attendu
cargo build --release                        # système courant
../tools/ci/test-wine.sh                     # compilé pour Windows ARM64, testé sous Wine (Docker)
```

## Utilisation de `lw-daemon`

```sh
cargo build --release
target/release/lw-daemon ifaces                                   # nom système, IP, index, nom convivial
target/release/lw-daemon send --iface en7 --channel 4001 --format standard --advertise --name "MAC 1"
target/release/lw-daemon recv --iface en7 --channel 1             # stats chaque seconde
target/release/lw-daemon run --config lw-daemon.json --control    # format : src/config.rs
target/release/lw-daemon ctl status                               # canal de contrôle du service
```

Canal de contrôle ([ADR 0007](../docs/adr/0007-canal-de-controle.md)) : `--control` sans valeur publie celui du service installé (XPC sous macOS, socket Unix sous Linux, tube nommé sous Windows) ; `--control unix:/tmp/lw.sock` ou `pipe:NOM` pour un essai. Sous Windows, `lw-daemon service` est lancé par le gestionnaire de services ([windows/README.md](../windows/README.md)).

Formats : `standard` (240 éch.), `aes67` (48), `livestream` (12), `surround` (60, 8 canaux, 239.196). `--tos 136` pour AF41. Sur une vraie NIC, le bouclage multicast est coupé : on ne reçoit pas ses propres flux.

macOS, binaire universel (plancher ADR 0004, mesuré : x86_64 `LC_VERSION_MIN_MACOSX` 10.13, arm64 `minos` 11.0) :

```sh
cargo build --release --target x86_64-apple-darwin && cargo build --release --target aarch64-apple-darwin
lipo -create -output target/lw-daemon-universal target/{x86_64,aarch64}-apple-darwin/release/lw-daemon
```

## Cadencement temps réel

Les threads d'émission passent en temps réel et attendent l'échéance sans attente active : `THREAD_TIME_CONSTRAINT_POLICY` et `mach_wait_until` sous macOS, `SCHED_FIFO` et `clock_nanosleep` sous Linux, MMCSS « Pro Audio » et minuteur haute résolution sous Windows. Mesures ci-dessous sous macOS. Une attente active ferait dépasser le budget de calcul déclaré, et le noyau rétrograderait le thread : c'est ce qu'a montré un premier essai, avec 2 s de trous. Retard maximal mesuré sur `lo0`, 5 s par mesure, 10 cœurs saturés par `yes` pour la charge :

| Flux | Normal, repos | Normal, charge | Temps réel, repos | Temps réel, charge |
|---|---|---|---|---|
| AES67 (1 000 pkt/s) | 9,3 ms | 14 ms, 1 156 paquets en retard | 330 µs | 20 µs, 0 en retard |
| Livestream (4 000 pkt/s) | 1,1 ms | 12 ms, 13 703 en retard | 42 µs | 38 µs, 0 en retard |

`--no-rt` reproduit le mode normal pour comparaison.

## Périphérique virtuel (région partagée)

Contrat unique en C : [lw-sys/csrc/lw_shm.h](lw-sys/csrc/lw_shm.h) et `lw_shm.c`, compilés **à l'identique** par le plugin HAL (et demain par le pilote ASIO). Aucune disposition mémoire n'est dupliquée en Rust : le daemon passe par ces fonctions C.

- Région = en-tête de 4 Kio + anneau `TO_NET` (applications → réseau ; producteur : plugin) + anneau `FROM_NET` (réseau → applications ; producteur : daemon), float32 entrelacés, SPSC sans verrou, positions 64 bits, compteurs de débordement et de sous-alimentation.
- Horloge en seqlock : (temps hôte, position d'échantillon, rapport de vitesse) ; l'en-tête déclare l'horloge hôte (`mach_absolute_time`, `QueryPerformanceCounter` ou `CLOCK_MONOTONIC`). Le plugin s'en sert dans `GetZeroTimeStamp` (ADR 0003).
- Transfert : le client envoie `{"cmd":"attach"}` en demandant la région ; objet `xpc_shmem` joint à la réponse sous macOS, section dupliquée dans le processus du client sous Windows. Sous Linux, la région reste dans le daemon.
- Côté daemon, un thread temps réel cadencé à 1 ms publie l'horloge, mesure les crêtes par canal et, avec `"device": {"loopback": true}`, renvoie la sortie des applications vers leur entrée.

Configuration (`"device"`, actif par défaut avec `--xpc`) : `channels_to_net` et `channels_from_net` (8 par défaut, 64 au plus), `ring_frames` (8192 par défaut, soit 170 ms), `loopback`.

## Découverte et patch (objectif : enregistrer un canal Livewire, émettre sur un canal choisi)

```sh
lw-daemon ctl set-iface en7                     # interface Livewire (nom BSD ou convivial)
lw-daemon ctl sources                           # sources annoncées sur le réseau (canal, nom, terminal)
lw-daemon ctl patch-in --channel 21 --to 1,2    # canal Livewire 21 → entrées 1-2 du périphérique
lw-daemon ctl patch-out --from 1,2 --channel 4001 --name "MAC 1"   # sorties 1-2 → canal 4001
lw-daemon ctl unpatch-in --to 1,2 ; lw-daemon ctl unpatch-out --channel 4001
lw-daemon ctl status                            # flux, routes (amorçage, glissements), crêtes
lw-daemon discover --iface en7 --seconds 30     # découverte sans daemon
```

- **Découverte** : le daemon écoute les annonces (239.192.255.3:4001), cumule les pages d'annonce complète et envoie la requête `READ` aux terminaux inconnus. Si un terminal ne répond pas à la requête, ses sources n'apparaissent qu'à sa prochaine annonce complète (2 à 3 min au plus).
- **Patch à chaud** : chaque commande valide la nouvelle configuration, l'enregistre (`lw-daemon.json`) et recharge la session réseau sans toucher au périphérique : le plugin garde sa région. Patcher une entrée déjà occupée la libère d'abord.
- **Autorisation** : les commandes qui modifient le patch exigent root ou le groupe `admin` (UID effectif de l'appelant XPC) ; `status`, `sources` et `config` sont libres.
- **Horloges** : un flux reçu suit l'horloge de l'émetteur, le périphérique celle du Mac. Le tampon de gigue absorbe la gigue réseau et rattrape la dérive par glissement (jette ou réamorce, compteurs `slips` et `underruns` dans `status`). Un léger saut audible peut survenir rarement, jusqu'à l'asservissement d'horloge (ADR 0003, rééchantillonnage adaptatif).
- **Latence d'entrée** : tampon de gigue d'environ 12 ms, plus le bloc d'IO de l'application et une marge de 256 trames (5,3 ms) gérées par le plugin, qui jette l'audio en retard.

## Journal et contrôle

- Journal unifié : `log stream --info --predicate 'subsystem == "fr.francois-brille.openlw"'` (catégorie `daemon`) ; stderr en usage interactif.
- Contrôle XPC : `lw-daemon run --config … --xpc` publie le service Mach `fr.francois-brille.openlw.daemon`, qui doit être déclaré par launchd ([launchd/fr.francois-brille.openlw.daemon.plist](launchd/fr.francois-brille.openlw.daemon.plist), posé par l'installeur). Interrogation : `lw-daemon ctl status` (domaine système) ou `lw-daemon ctl status --user` (LaunchAgent de test). Protocole JSON : [lw-daemon/src/control.rs](lw-daemon/src/control.rs).
- Hors launchd, un service Mach ne peut pas être publié : les tests utilisent un écouteur XPC anonyme dans le même processus.

## Tests

- **Contrat** (`tests/vectors.rs`) : relit `docs/protocol/vectors/` (générés par `tools/lw/make_vectors.py`) et ré-encode les annonces et le SDP octet pour octet. Deux implémentations indépendantes, Python et Rust, doivent concorder.
- **Couche C** (`lw-sys`) : promotion temps réel, précision du sommeil temps réel, `os_log`, aller-retour XPC (50 requêtes), panique de gestionnaire confinée, service inconnu.
- **Région partagée** (`lw-sys/src/shm.rs`) : géométrie, unicité des extrémités, rebouclage et compteurs, stress SPSC de 200 000 trames à travers deux mappages réels transmis par XPC (sans déchirure), seqlock sur 200 000 lectures concurrentes.
- **Périphérique** (`lw-daemon/tests/device.rs`) : un faux plugin s'attache par XPC, lit l'horloge (48 kHz à ±1 ms près), écrit 0,5 s de sinusoïde et la relit identique, échantillon pour échantillon, par la boucle interne ; crêtes par canal à 0,1 dB près.
- **Patch** (`lw-daemon/tests/patch.rs`, `src/editor.rs`, `src/patch.rs`) : sur lo0, un terminal Livewire simulé émet sur le canal 21 ; `patch_input` par la commande de contrôle, rechargement de session, entrées 3-4 à −12 dBFS et autres muettes ; `patch_output` des sorties 1-2 vers le canal 4005, reçu ailleurs à −6 dBFS sans perte ; appelant non autorisé refusé.
- **Découverte** (`lw-daemon/tests/discovery.rs`) : terminal simulé à 10 sources (2 pages) reconstitué.
- **Contrôle** (`lw-daemon/src/control.rs`) : requêtes `status`, `ping`, erreurs ; même chose de bout en bout par XPC.
- **Boucle locale** (`lw-daemon/tests/loopback.rs`) : sur `lo0`, flux Standard, AES67 et surround émis puis reçus (aucune perte, Δts, charge, SSRC, niveau −20 dBFS) ; annonce de 10 sources relue en 2 pages + 1 courte.
- **Robustesse** (`tests/robustness.rs`) : 20 000 entrées aléatoires et 20 000 mutations de paquets valides passées à tous les décodeurs. C'est un substitut de `cargo-fuzz`, qui exige la toolchain nightly.
