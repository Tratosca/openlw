# Pilote audio Windows d'OpenLW

<img src="../../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="64" align="right">

Pilote ASIO® en mode utilisateur : les logiciels audio compatibles ASIO (stations audionumériques, logiciels de diffusion) le voient sous le nom **« OpenLW »**. Décision : [ADR 0008](../../docs/adr/0008-audio-windows.md).

- DLL COM in-process `OpenLWDriver.dll`, Windows 10 22H2 et 11, x64 et ARM64.
- 48 kHz, échantillons float 32 bits, tampons de 64 à 2048 trames (256 par défaut).
- Canaux et noms : ceux du périphérique OpenLW, réglés dans l'app (patch des entrées et des sorties).
- Un logiciel hôte à la fois ; un second reçoit le message « OpenLW est déjà utilisé par une autre application (…) ».

## Licence

Ce dossier est sous **licence GPL version 3** ([LICENSE](LICENSE)) : le pilote est construit avec le SDK ASIO de Steinberg, utilisé sous GPLv3. Le reste d'OpenLW est sous licence Apache 2.0 ; le pilote y fait appel (couche système et région partagée de `daemon/lw-sys/csrc`, compatibles GPLv3) et ne communique avec le service que par IPC.

Le SDK n'est pas versionné : `fetch-sdk.ps1` (Windows) ou `fetch-sdk.sh` le télécharge depuis steinberg.net et vérifie son empreinte SHA-256.

## Construire

Prérequis : Visual Studio Build Tools (C++), CMake 3.20 ou plus.

```powershell
powershell -ExecutionPolicy Bypass -File windows\driver\fetch-sdk.ps1
cmake -S windows\driver -B build\driver -A ARM64      # ou -A x64
cmake --build build\driver --config Release
```

Installation de développement (PowerShell en administrateur, service OpenLW démarré, voir [windows/README.md](../README.md)) :

```powershell
Copy-Item build\driver\Release\OpenLWDriver.dll "$env:ProgramFiles\OpenLW\"
regsvr32 "$env:ProgramFiles\OpenLW\OpenLWDriver.dll"       # retrait : regsvr32 /u …
```

L'enregistrement crée `HKCR\CLSID\{4F9DD084-E18A-4E0E-8D38-C2854029E846}` et `HKLM\SOFTWARE\ASIO\OpenLW`. Le bouton « Panneau de configuration » des logiciels hôtes ouvre l'app OpenLW (`OpenLW.exe`, installée à côté du pilote).

## Tester

`openlw-driver-test.exe` charge le pilote comme un logiciel hôte, joue une sinusoïde sur deux sorties et vérifie qu'elle revient sur deux entrées par la boucle interne du service (configuration `"device": {"loopback": true}`) :

```powershell
lw-daemon.exe run --config loopback.json --control
build\driver\Release\openlw-driver-test.exe "$PWD\build\driver\Release\OpenLWDriver.dll"
```

Sans machine Windows : `tools/ci/test-wine.sh` (ARM64, Wine natif sur hôte arm64) construit le pilote avec llvm-mingw et déroule le même essai, plus le refus d'un second processus. La CI le fait sur Windows x64 et ARM64 avec MSVC.

## Fonctionnement

- `init` : connexion au service par le tube nommé, lecture de la géométrie (canaux, noms, marge de latence), `attach` avec demande de la région ; le service duplique la section partagée dans le processus hôte. La région doit déclarer l'horloge `QueryPerformanceCounter`.
- Thread audio MMCSS « Pro Audio » : chaque échange de tampons est calé sur l'horloge publiée par le service ; entrées lues dans l'anneau réseau → applications avec la marge de latence du service (rattrapage au-delà de deux marges), sorties écrites dans l'anneau applications → réseau. Mode `ASIOTime` quand l'hôte le prend en charge ; horodatage dérivé de `timeGetTime()`, comme le demande la spécification ASIO sous Windows.
- Surveillance : toutes les 2 s, une seconde connexion relit la géométrie ; périphérique recréé (nombre de canaux, génération) ou service perdu → `kAsioResetRequest` à l'hôte.
- Libération : `detach` au service à la destruction du pilote ; si le processus hôte disparaît sans prévenir, le service le détecte au prochain `attach`.

## Limites

- Les applications sans ASIO (navigateurs, visioconférence) ne voient pas OpenLW : voir le pilote noyau WaveRT dans [docs/roadmap.md](../../docs/roadmap.md).
- Hôtes x64 émulés sur Windows ARM64 : le pilote ARM64 ne se charge pas dans un processus x64. Une DLL ARM64X (code ARM64 et ARM64EC) est prévue ; d'ici là, les hôtes doivent être natifs ARM64 sur ces machines.
- Essais réels à faire : logiciels hôtes du commerce (Reaper, logiciels de diffusion), cohabitation avec le driver Axia.

---
ASIO is a registered trademark of Steinberg Media Technologies GmbH.
