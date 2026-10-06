# OpenLW for Windows

<img src="../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="48" align="right">

Windows 10 22H2 and 11, x64 and ARM64. Audio device: ASIO® driver, displayed as “OpenLW” in host applications ([ADR 0008](../docs/adr/0008-audio-windows.md)).

| Component | Directory | Status |
|---|---|---|
| Network service | `daemon/` (`lw-daemon service`) | Implemented; validation on real Windows pending |
| ASIO driver | [`windows/driver`](driver/README.md) (`OpenLWDriver.dll`, GPLv3, Steinberg SDK downloaded at build time) | Implemented; full Wine ARM64 test; real Windows and commercial hosts remain to be tested |
| OpenLW app | `windows/app` (WinUI 3, C#) | Planned |
| Installer | `windows/installer` (x64 and ARM64 MSI) | Planned |

## Development service installation

Prerequisites: Rust (MSVC target), Visual Studio Build Tools with the C compiler.

```powershell
cd daemon
cargo build --release
# Elevated PowerShell:
New-Item -ItemType Directory -Force "$env:ProgramFiles\OpenLW" | Out-Null
Copy-Item target\release\lw-daemon.exe "$env:ProgramFiles\OpenLW\"
sc.exe create OpenLW binPath= "\"$env:ProgramFiles\OpenLW\lw-daemon.exe\" service" start= auto
New-EventLog -LogName Application -Source OpenLW        # Optional: Event Log source
net localgroup "OpenLW Users" /add
net localgroup "OpenLW Users" $env:USERNAME /add                # Allow changes without elevation
sc.exe start OpenLW
```

- Configuration: `%ProgramData%\OpenLW\lw-daemon.json` (created on first startup).
- Log: `%ProgramData%\OpenLW\Logs\lw-daemon.log`; errors also go to the Event Log (OpenLW source).
- Control: `lw-daemon ctl status` (named pipe `\\.\pipe\fr.francois-brille.openlw.daemon`). New `OpenLW Users` group membership takes effect after signing in again.
- Removal: `sc.exe stop OpenLW`, `sc.exe delete OpenLW`, then remove the directories.

## Coexistence with the Axia driver

OpenLW shares Livewire ports (`SO_REUSEADDR`) without exclusive binding; use a distinct terminal name (`lw-daemon ctl set-advanced --name …`). This remains to be verified on a machine with both installed.

## Testing without a Windows machine

`tools/ci/test-wine.sh` (ARM64, native Wine on an arm64 host) or `tools/ci/test-wine.sh x64` tests the system layer, control channel, shared region, and device. Wine does not enumerate the container's network adapters: network tests run in CI on real Windows.

---
ASIO is a registered trademark of Steinberg Media Technologies GmbH.
