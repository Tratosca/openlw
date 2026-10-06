# OpenLW pour Windows

<img src="../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="48" align="right">

Windows 10 22H2 et 11, x64 et ARM64. Périphérique audio : pilote ASIO®, affiché « OpenLW » dans les logiciels hôtes ([ADR 0008](../docs/adr/0008-audio-windows.md)).

| Composant | Dossier | État |
|---|---|---|
| Service réseau | `daemon/` (`lw-daemon service`) | prêt, à valider sur Windows réel |
| Pilote ASIO | [`windows/driver`](driver/README.md) (`OpenLWDriver.dll`, GPLv3, SDK Steinberg téléchargé à la compilation) | fait, essai complet sous Wine ARM64 ; Windows réel et hôtes du commerce à essayer |
| App OpenLW | `windows/app` (WinUI 3, C#) | à faire |
| Installeur | `windows/installer` (MSI x64 et ARM64) | à faire |

## Service, installation de développement

Prérequis : Rust (cible MSVC), Visual Studio Build Tools avec le compilateur C.

```powershell
cd daemon
cargo build --release
# PowerShell en administrateur :
New-Item -ItemType Directory -Force "$env:ProgramFiles\OpenLW" | Out-Null
Copy-Item target\release\lw-daemon.exe "$env:ProgramFiles\OpenLW\"
sc.exe create OpenLW binPath= "\"$env:ProgramFiles\OpenLW\lw-daemon.exe\" service" start= auto
New-EventLog -LogName Application -Source OpenLW        # facultatif : source du journal des événements
net localgroup OpenLW /add
net localgroup OpenLW $env:USERNAME /add                 # modifications sans session élevée
sc.exe start OpenLW
```

- Configuration : `%ProgramData%\OpenLW\lw-daemon.json` (créée au premier démarrage).
- Journal : `%ProgramData%\OpenLW\Logs\lw-daemon.log` ; erreurs aussi dans le journal des événements (source OpenLW).
- Contrôle : `lw-daemon ctl status` (tube nommé `\\.\pipe\fr.francois-brille.openlw.daemon`). La nouvelle appartenance au groupe `OpenLW` ne vaut qu'après une nouvelle ouverture de session.
- Retrait : `sc.exe stop OpenLW`, `sc.exe delete OpenLW`, puis suppression des dossiers.

## Coexistence avec le driver Axia

OpenLW partage les ports Livewire (`SO_REUSEADDR`) et n'en occupe aucun en exclusivité ; donner un nom de terminal distinct (`lw-daemon ctl set-advanced --name …`). À vérifier sur une machine équipée des deux.

## Tests sans machine Windows

`tools/ci/test-wine.sh` (ARM64, Wine natif sur hôte arm64) ou `tools/ci/test-wine.sh x64` : couche système, canal de contrôle, région partagée et périphérique. Wine n'énumère pas les cartes réseau du conteneur : les tests réseau tournent en CI, sur Windows réel.

---
ASIO is a registered trademark of Steinberg Media Technologies GmbH.
