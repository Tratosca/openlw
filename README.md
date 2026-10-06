# OpenLW

OpenLW is an open-source audio driver for Livewire®-compatible and AES67 audio-over-IP networks. It connects desktop audio applications to network sources and destinations through a virtual audio device, source discovery, and an input/output patch matrix.

| Platform | Architectures | Audio device | App | Status |
|---|---|---|---|---|
| macOS 10.13 and later | Intel, Apple Silicon | CoreAudio (all applications) | OpenLW (AppKit, Liquid Glass on macOS 26) | Available |
| Windows 10 22H2 and 11 | x64, ARM64 | ASIO® driver, displayed as “OpenLW” (ASIO-compatible applications) <img src="docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="40"> | OpenLW (WinUI 3, planned) | In progress; network service implemented |
| Linux (PipeWire 0.3.49+) | x86_64, ARM64 | PipeWire nodes (PipeWire, PulseAudio, and JACK applications) | OpenLW (GTK4, planned) | In progress; network service and PipeWire nodes implemented |

See the [roadmap](docs/roadmap.md), [Windows guide](windows/README.md), and [Linux guide](linux/README.md) for platform-specific progress and validation limits. The following usage instructions describe macOS.

## Features

- **Virtual audio device:** “OpenLW”, or separate “OpenLW In” and “OpenLW Out” devices, with 1–16 stereo channels in each direction.
- **Receive:** discover advertised network sources, patch a Livewire channel to an input pair, and preview sources through the Mac's audio output.
- **Transmit:** send each output pair on the selected Livewire channel, using the selected source name, and advertise it to other devices.
- **Network selection:** automatically use the interface receiving Livewire advertisements; apply changes without interrupting unrelated streams.
- **OpenLW app:** manage patches, transmission, meters, settings, and uninstallation.

## Install and start on macOS

Open `OpenLW-<version>.pkg` and follow the installer. Then:

1. Connect the Mac to the Livewire network.
2. Open **OpenLW** from Applications.
3. To transmit, select “OpenLW” as the Mac's or application's output, enter the destination channel in OpenLW, and enable transmission (the current app labels this checkbox **Diffuser**).
4. To record, click the source's cell in the input matrix, then record from “OpenLW” in your audio application.

To uninstall, use the OpenLW menu's uninstall command (currently **Désinstaller OpenLW**).

The package is not yet signed or notarized. macOS requires confirmation when opening it: right-click > Open, or use System Settings > Privacy & Security.

## Build from source

The network service requires Rust 1.82 or later and a C compiler (Clang, GCC, or MSVC).

```sh
(cd daemon && cargo build --release)    # daemon/target/release/lw-daemon
```

For macOS, install Xcode with the macOS 26 SDK or later and the Rust targets `x86_64-apple-darwin` and `aarch64-apple-darwin`.

```sh
macos/scripts/build-all.sh          # Universal service, plugin, and app binaries
macos/installer/build-pkg.sh        # build/OpenLW-<version>.pkg
```

Platform and component guides:

- [Network service and configuration](daemon/README.md)
- [macOS audio plugin](macos/plugin/README.md), [app](macos/app/README.md), and [installer](macos/installer/README.md)
- [Windows](windows/README.md) and [Windows audio driver](windows/driver/README.md)
- [Linux](linux/README.md)

## Validate changes

```sh
(cd daemon && cargo test && cargo clippy --all-targets)
make -C macos/plugin test
python3 -m pytest -q tools/lw/tests
tools/ci/test-wine.sh               # Windows build under Wine (Docker)
```

[CI](.github/workflows/ci.yml) tests the daemon on macOS, Linux, and Windows, on x86_64 and ARM64. Container and Wine results do not replace validation on target systems. When contributing, use English for documentation and code comments, and keep observed behavior, implementation choices, and unverified assumptions distinct.

## Repository layout

| Directory | Contents |
|---|---|
| `daemon/` | Cross-platform Rust network service: RTP, advertisements, discovery, patching, and control channel |
| `macos/plugin/` | CoreAudio AudioServerPlugIn in C: the virtual audio device |
| `macos/app/` | OpenLW app in Swift and AppKit |
| `macos/installer/` | macOS `.pkg` installer |
| `windows/` | Windows service, audio driver, app, and installer (in progress) |
| `linux/` | Linux systemd unit, app, and packages (in progress) |
| `docs/protocol/` | Network specification: what OpenLW sends and accepts |
| `docs/adr/` | Architecture decisions |
| `tools/lw/` | Python codecs, test transmitters, and capture analysis tools |
| `tools/wireshark/` | Wireshark dissector |
| `tools/ci/` | Windows tests under Wine (Docker) |

## Limitations

- No network-clock synchronization yet (PTP or Livewire clock). Buffer slips compensate for clock drift, with occasional brief interruptions; see [ADR 0003](docs/adr/0003-horloge.md).
- Surround and AES67 have not been verified with third-party devices; see [open questions](docs/protocol/open-questions.md).
- macOS 10.13 has not been tested on a target machine.

## License and trademarks

OpenLW is licensed under Apache License 2.0, except for the Windows audio driver, which is licensed under GPLv3 and built with Steinberg's ASIO SDK. See [LICENSE](LICENSE), [NOTICE](NOTICE), and the [driver license](windows/driver/LICENSE).

Livewire, Livewire+, and Axia are trademarks of TLS Corp. (Telos Alliance). OpenLW is an independent project, neither affiliated with nor endorsed by TLS Corp.; these names indicate compatibility only. OpenLW contains no TLS Corp. code, binaries, or documents.

ASIO is a registered trademark of Steinberg Media Technologies GmbH.

Copyright 2026 François Brille (Tratosca).
