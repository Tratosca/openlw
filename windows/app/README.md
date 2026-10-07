# OpenLW app for Windows

<img src="../../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="48" align="right">

WinUI 3 configuration app (C#, .NET 10, Windows App SDK, unpackaged and self-contained), x64 and ARM64. Same features and layout as the macOS app ([macos/app](../../macos/app/README.md)), on a Mica backdrop:

| Section | Contents |
|---|---|
| Livewire network | Connection state, interface (automatic or forced), source advertisement |
| Audio driver | How to select the ASIO® driver “OpenLW” in host applications; ASIO Compatible logo and trademark notice |
| Inputs | Patch matrix (discovered, manual and unadvertised sources × driver input channels, headed per pair with meters, state and a coupling link); headphone preview. See below |
| Outputs | One row per output pair: meter, Livewire channel, advertised name, format, transmission |
| Advanced settings | Advertised name, receive latency, DSCP |

## Input matrix

Same model as the macOS app (duplex layout only on Windows): the daemon stores crosspoints (`taps`: driver input ← stream channel(s)) and the pairs that are not coupled (`uncoupled_inputs`).

- **Coupled pair** (the default): one target over the two inputs. A click patches the source in stereo there, a click on the patched pair releases it. A source may feed several pairs.
- **Link button** in the pair header: uncouples or couples the pair. Uncoupling keeps the audio (a stereo patch becomes left on the first input, right on the second). Coupling makes the first input's source stereo on the pair and releases the rest; the app asks first only when other sources' patches would be released.
- **Uncoupled pair**: each input has two halves, top = left (L), bottom = right (R). A click adds or removes that side of the source on that input; both halves give L+R (mono sum, −6 dB). A source may feed several inputs, so stereo on inputs 2-3 is possible.
- **Surround source**: a click on any pair (coupled or not) patches its 8 channels from the pair start; a click on a patched pair releases the whole block.

The source columns (preview, channel, name, origin) stay fixed on the left; the cells scroll horizontally when the pairs do not fit (up to 16 pairs, 32 inputs), so the window keeps its width.

## Languages

English by default, French when the Windows display language is French. Strings are MRT Core resources: `Strings\en-US\Resources.resw` (keys and English text, same wording as the macOS and Linux apps) and `Strings\fr-FR\Resources.resw`, compiled into `OpenLW.pri` next to the executable; XAML uses `x:Uid`, code uses `Loc.S` / `Loc.F`. The environment variable `OPENLW_UI_LANGUAGE` (`en-US` or `fr-FR`) forces the language. Messages from the service and channel names published by the service stay in English.

## Service connection

Named pipe `\\.\pipe\fr.francois-brille.openlw.daemon`, one JSON request per line ([ADR 0007](../../docs/adr/0007-control-channel.md)). The app opens the pipe with exactly the rights granted to authenticated users and at Identification level. It runs as the signed-in user (`asInvoker`): changes are allowed to members of the local `OpenLW Users` group (added by the installer) and to elevated administrators; otherwise the service error is shown.

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
