# OpenLW app for Windows

<img src="../../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="48" align="right">

WinUI 3 configuration app (C#, .NET 10, Windows App SDK, unpackaged and self-contained), x64 and ARM64. Same features and layout as the macOS app ([macos/app](../../macos/app/README.md)), on a Mica backdrop:

| Section | Contents |
|---|---|
| Réseau Livewire | Connection state, interface (automatic or forced), source advertisement |
| Pilote audio | How to select the ASIO® driver “OpenLW” in host applications; ASIO Compatible logo and trademark notice |
| Entrées | Patch matrix (discovered, manual and unadvertised sources × input pairs), per-pair meters and state, headphone preview |
| Sorties | One row per output pair: meter, Livewire channel, advertised name, format, transmission |
| Réglages avancés | Advertised name, receive latency, DSCP |

User-facing strings are in French, like the macOS app.

## Service connection

Named pipe `\\.\pipe\fr.francois-brille.openlw.daemon`, one JSON request per line ([ADR 0007](../../docs/adr/0007-canal-de-controle.md)). The app opens the pipe with exactly the rights granted to authenticated users and at Identification level. It runs as the signed-in user (`asInvoker`): changes are allowed to members of the local `OpenLW` group (added by the installer) and to elevated administrators; otherwise the service error is shown.

## Preview

The headphone button joins the source multicast group on the Livewire interface (shared port 5004), decodes RTP L24/L16 and plays the first two channels on the default Windows output (shared WASAPI through NAudio, MIT license): 30 ms buffer before playback, excess above 200 ms discarded.

## Build

Prerequisites: .NET 10 SDK, Visual Studio Build Tools with the Windows SDK.

```powershell
dotnet publish windows\app\OpenLW.csproj -c Release -r win-arm64   # or win-x64
```

Output: `windows\app\bin\Release\net10.0-windows10.0.19041.0\win-arm64\publish\OpenLW.exe` and its runtime files (the installer packages this folder).

## Dependencies

- Microsoft.WindowsAppSDK (MIT)
- NAudio.Wasapi (MIT)

---
ASIO is a registered trademark of Steinberg Media Technologies GmbH.
