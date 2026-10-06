# OpenLW

App de réglage du driver : interface réseau, nombre de canaux, patch des sources Livewire vers les entrées du Mac, diffusion des sorties du Mac sur des canaux Livewire, vumètres. AppKit en code, sans storyboard. Elle pilote le service réseau par XPC (`fr.francois-brille.openlw.daemon`) sans jamais le nommer à l'écran : l'utilisateur voit un réseau, des canaux et le périphérique « OpenLW ».

## Construire

```
make -C app            # build/OpenLW.app, universelle, signature ad hoc
make -C app check-min  # plancher par tranche : x86_64 10.13, arm64 11.0
make -C app snapshot   # lance l'app et rend sa fenêtre dans app/build/snapshot.png
```

Prérequis : Xcode (SDK macOS 26 ou plus, pour `NSGlassEffectView`). Sous macOS 10.14.4, le système ne fournit pas le runtime Swift : `swift-stdlib-tool` le copie dans `Contents/Frameworks`. Sur un système plus récent, `/usr/lib/swift` est chargé en premier.

`scripts/build-all.sh` construit l'app avec le reste ; `sudo scripts/install-dev.sh` la copie dans `/Applications`.

## Fenêtre

| Zone | Contenu | Commandes XPC |
|---|---|---|
| Réseau Livewire | interface utilisée ou recherche en cours ; menu « Automatique » (interface qui entend des annonces Livewire) ou interface Ethernet forcée ; annonce des sorties | `status` (5 Hz), `ifaces`, `set_iface`, `set_advertise` |
| Entrées du Mac | nombre de canaux reçus (1 à 16 stéréo) ; entrée par défaut du Mac et bouton « Utiliser OpenLW » ; grille : sources découvertes (ADV), flux configurés non annoncés et canaux saisis, en lignes ; paires d'entrées du périphérique en colonnes. Un clic patche ou libère. Vumètre et état par paire (libre, en attente, audio reçu) | `sources`, `config` (2 s), `patch_input`, `unpatch_input` |
| Sorties du Mac | nombre de canaux diffusés (1 à 16 stéréo) ; sortie par défaut du Mac et bouton « Utiliser OpenLW » ; une ligne par paire de sorties : vumètre, canal, nom annoncé, format (Standard 5 ms, AES67 1 ms, Livestream 0,25 ms), case Diffuser | `set_device_channels`, `patch_output`, `unpatch_output` |

Réduire le nombre de canaux retire les patchs devenus hors plage, après confirmation. Le périphérique est recréé : le son des applications qui l'utilisent s'interrompt un instant.

**Réglages avancés** (section repliable, état mémorisé) :

| Réglage | Valeurs | Effet |
|---|---|---|
| Nom annoncé du Mac | 32 caractères au plus ; vide = nom de l'ordinateur | `ATRN` des annonces, translittéré en ASCII |
| Latence de réception | Faible, Normale (défaut), Sûre | tampon de gigue 6, 12 ou 24 ms et marge d'entrée du plugin 128, 256 ou 512 trames : ≈ 8, 17 ou 35 ms ajoutées ; réserve d'émission 256, 512 ou 1024 trames |
| Priorité réseau (DSCP) | EF 46 (défaut), AF41 34, aucune 0 | marquage des flux audio émis |

Commande XPC : `set_advanced` (`terminal_name`, `latency`, `dscp`) ; en ligne de commande, `lw-daemon ctl set-advanced`.

Menu OpenLW > Désinstaller OpenLW : lance `/Library/Application Support/OpenLW/uninstall.sh` avec les droits administrateur (posé par l'installeur).

**Écouter** : le bouton casque en tête de ligne joue la source sur la sortie audio par défaut du Mac, sans la patcher ; un niveau remplace alors la provenance. Une seule source à la fois ; un second clic arrête l'écoute. L'app reçoit elle-même le flux multicast sur l'interface de la session (le daemon et l'app partagent le port 5004 grâce à `SO_REUSEPORT`). Jouée en stéréo à 48 kHz via AudioQueue, avec 30 ms de tampon ; d'une source surround, seuls les canaux 1 et 2 sont joués. L'écoute est refusée quand la sortie par défaut du Mac est « OpenLW » (elle repartirait vers le réseau).

**Lignes de la grille** : sources découvertes, canaux saisis (mémorisés entre deux lancements) et flux configurés mais non annoncés. Le bouton ✕ retire une ligne saisie ou non annoncée ; si elle est patchée, ses entrées sont libérées.

**Périphérique audio** : présentation en un périphérique « OpenLW » (entrée et sortie, nom fixe, défaut) ou en deux périphériques « OpenLW In » et « OpenLW Out ». Avec deux périphériques, la case « Nommer les périphériques d'après les canaux patchés » donne par exemple « OpenLW In (2 - Studio A) » et « OpenLW Out (31 - Mac 1-2) ». Les canaux portent toujours le nom de leur source (« 2 - Studio A G »), visible dans Configuration audio et MIDI et dans les applications qui l'affichent. Audacity mémorise le périphérique par son nom : après un changement de présentation ou de nom, il faut le sélectionner de nouveau.

Une source surround occupe 8 entrées à partir de la paire cliquée. Changer le canal d'une sortie émise arrête l'ancien canal, puis émet le nouveau. Les sources de test sans sorties associées (générateur de sinusoïde de la config) ne sont pas affichées.

Modifier le patch exige que l'utilisateur soit administrateur (groupe `admin`) : le daemon vérifie l'uid de l'appelant. Les erreurs du daemon s'affichent sous l'en-tête.

## Apparence

Panneaux en Liquid Glass (`NSGlassEffectView`) sur macOS 26 et plus, `NSVisualEffectView` en dessous. Fond de fenêtre en vibrance (`.sidebar`). Couleurs système, mode sombre automatique.

## Essai sans installation

Le daemon peut tourner en LaunchAgent sous un autre nom de service. L'app s'y connecte avec :

```
"app/build/OpenLW.app/Contents/MacOS/OpenLW" --user --service fr.francois-brille.openlw.daemon.dev
```

Banc simulé sur lo0 (une seule machine) :

```
python3 tools/lw/emit_adv.py --iface lo0 --ip 127.0.0.2 --channel 4001 --name "STUDIO-A PGM" --terminal STUDIO-A
build/lw-daemon send --iface lo0 --channel 4001 --level -12
```

Validé ainsi : mode automatique en recherche ; puis lo0 forcée, passage à 2 canaux dans chaque sens (génération 2 du périphérique), sources découvertes, patch 4001 → entrées 3-4, vumètre à −12 dBFS, « audio reçu », sorties 1-2 diffusées sur 4005. Écoute de 4001 (option `--listen 4001`) : niveau reçu affiché pendant que le daemon alimente toujours les entrées 3-4.

## Reste à faire

- Valider sur 10.13 (VM ou Mac Intel) : non testé.
- Restitution de l'écoute sur haut-parleurs : file AudioQueue démarrée sans erreur, son non contrôlé à l'oreille.
- Clics réels dans la grille et la liste : le chemin XPC est celui de `lw-daemon ctl`, validé ; l'interaction souris reste à valider à l'écran.
- Signature Developer ID et notarisation.
