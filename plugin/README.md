# plugin — périphérique CoreAudio (AudioServerPlugIn)

Plugin HAL chargé par `coreaudiod` : périphérique **« OpenLW »** (fabricant « François Brille »), float32 à 48 kHz. Nombre de canaux : fixé par le daemon (section `device` de sa config, réglée depuis OpenLW, 1 à 32 par sens). Les clés `LWChannelsFromNet` et `LWChannelsToNet` de [Info.plist](Info.plist) (2 et 2) ne servent qu'au démarrage, avant la première réponse du daemon.

- **Latence d'entrée** : bornée par le plugin, qui connaît la taille de bloc de l'hôte (512 à 4096 trames et plus). Le daemon remplit l'anneau tant qu'il y a de la place. Avant de lire N trames, le plugin attend d'en avoir N + 256 (amorçage, silence en attendant), puis jette l'excédent au-delà de N + 512. L'anneau est vidé au démarrage de l'IO (audio ancien). Corrige l'enregistrement par blocs de 2048 ou 4096 trames (Audacity), qui recevait du silence au-delà de 1024 trames.
- **Présentation** : un périphérique duplex « OpenLW » (objet 2, UID `fr.francois-brille.openlw.device`), ou deux périphériques « OpenLW In » (5, `….device.in`) et « OpenLW Out » (7, `….device.out`), selon `layout` dans la réponse `geometry`. Chaque périphérique a son horloge d'IO ; ils partagent la région du daemon. Un changement du nombre de canaux est appliqué périphérique par périphérique (sens par sens), et la région détachée n'est démappée qu'au détachement suivant, l'IO de l'autre périphérique pouvant encore la lire.
- **Noms** : la réponse `geometry` porte le nom du périphérique (« OpenLW », ou « OpenLW In (2 - Studio A) » si l'option est activée) et le nom de chaque canal (`kAudioObjectPropertyElementName`, ex. « 2 - Studio A G »). Un changement est signalé à l'hôte par `PropertiesChanged`.
- **Nombre de canaux à chaud** : une file de surveillance interroge `geometry` toutes les 2 s. Si le nombre de canaux ou la génération de la région change, le plugin demande un changement de configuration à l'hôte (`RequestDeviceConfigurationChange`) ; dans `PerformDeviceConfigurationChange`, il détache la région et applique les nouveaux nombres ; le `StartIO` suivant se rattache. La même file rattache la région quand l'IO tourne sans elle (daemon démarré après l'application).

- **Audio** : par la région partagée du daemon ([../daemon/lw-sys/csrc/lw_shm.h](../daemon/lw-sys/csrc/lw_shm.h), compilé tel quel ici), obtenue au premier `StartIO` par XPC (requête `attach`, service `fr.francois-brille.openlw.daemon`, déclaré dans `AudioServerPlugIn_MachServices`).
- **Sans daemon** : le périphérique reste visible et silencieux, l'erreur est journalisée.
- **Temps réel** : `GetZeroTimeStamp` et `DoIOOperation` ne verrouillent rien, n'allouent rien et ne font aucun appel système.
- **Journal** : `log stream --info --predicate 'subsystem == "fr.francois-brille.openlw" AND category == "plugin"'`.

## Construire et tester (sans installation)

```sh
make            # build/OpenLW.driver : universel, signé en ad hoc
make check-min  # x86_64 LC_VERSION_MIN_MACOSX 10.13, arm64 minos 11.0
make test       # banc d'essai : charge le bundle comme coreaudiod + simule le daemon
```

Le banc d'essai ([test/harness.c](test/harness.c)) fait 32 vérifications :
- chargement CFPlugIn et fabrique ;
- propriétés vues par le HAL ;
- silence sans daemon ;
- attachement XPC, puis 100 cycles de 512 trames identiques dans chaque sens ;
- période de l'horodatage zéro (16 384 trames = 0,341333 s).

## Installer (sudo, redémarre coreaudiod)

```sh
make install     # copie dans /Library/Audio/Plug-Ins/HAL, root:wheel, puis killall coreaudiod
make uninstall   # retire et redémarre coreaudiod
```

Vérification : le périphérique apparaît dans Configuration audio et MIDI et dans Réglages Système > Son. Pour qu'il transporte du son, le daemon doit tourner en LaunchDaemon avec le service XPC ([../daemon/launchd/](../daemon/launchd/)) : sans launchd, un service Mach ne peut pas être publié.

`killall coreaudiod` coupe brièvement l'audio de toutes les applications. La signature ad hoc suffit pour un essai local ; la distribution passera par Developer ID et la notarisation (M8).

## Validation réelle (2026-10-05, macOS 27, Apple Silicon)

Installé avec `scripts/install-dev.sh` (daemon en LaunchDaemon, config de dev avec boucle interne) :
- **Périphérique** : « OpenLW » visible par le HAL (`system_profiler SPAudioDataType` : 8 entrées, 8 sorties, 48 kHz, transport virtuel, utilisable comme sortie par défaut). Le plugin tourne dans `Core-Audio-Driver-Service.helper` et atteint le service XPC du daemon depuis son bac à sable.
- **Boucle audio** : un client CoreAudio joue une sinusoïde de 1 kHz à −12 dBFS sur les canaux 1 et 2 et enregistre l'entrée.

  | Mesure | Résultat |
  |---|---|
  | Trames jouées / reçues | 144 384 / 144 384 (3 s) |
  | Crête en entrée | −12,0 dBFS sur les canaux 1 et 2, silence sur les canaux 3 à 8 |
  | Latence aller-retour | 10,7 ms (un tampon de 512 trames) |
  | Pertes | 0 débordement ; 512 trames de silence à l'amorçage |

- Remarque : `say -a` ne liste pas tous les périphériques et plante sur un périphérique au nom court (« FM ») : c'est un bogue de `say`, sans lien avec le plugin. Pour tester, utiliser une application qui choisit son périphérique, ou un client CoreAudio.

## Limites connues

- Anneau en file d'attente (FIFO) : la latence suit le remplissage ; pas encore d'indexation par temps d'échantillon (`mInputTime` / `mOutputTime`).
- Horloge nominale (horloge hôte à 48 kHz) : l'asservissement sur l'horloge réseau publiée par le daemon (`rate_scalar`) reste à faire.
- Attachement au `StartIO` seulement : si le daemon redémarre pendant l'IO, le plugin reste sur l'ancienne région (silence) jusqu'au prochain arrêt puis démarrage de l'IO.
