# OpenLW

Driver configuration app: network interface, channel count, patches from Livewire sources to Mac inputs, Mac output transmission on Livewire channels, and meters. Programmatic AppKit, no storyboard. It controls the network service through XPC (`fr.francois-brille.openlw.daemon`) without naming the service on screen: users see a network, channels, and the “OpenLW” device.

## Build

```
make -C macos/app            # build/OpenLW.app, universal, ad hoc signing
make -C macos/app check-min  # Minimum per slice: x86_64 10.13, arm64 11.0
make -C macos/app snapshot   # Launch the app and render its window to macos/app/build/snapshot.png
```

Prerequisite: Xcode (macOS 26 SDK or later for `NSGlassEffectView`). Before macOS 10.14.4, the system does not provide the Swift runtime: `swift-stdlib-tool` copies it into `Contents/Frameworks`. On newer systems, `/usr/lib/swift` is loaded first.

`macos/scripts/build-all.sh` builds the app with the other components; `sudo macos/scripts/install-dev.sh` copies it to `/Applications`.

## Languages

English by default, French when the system language is French (`Resources/fr.lproj/Localizable.strings`; keys are the English text, wrapped in `L()` in the sources, placeholders `%@` only). `make check-l10n` checks that every string is translated with the same placeholders (also run by CI). Daemon error messages and the channel names published to Core Audio stay in English. Test another language without changing the system: `OpenLW.app/Contents/MacOS/OpenLW -AppleLanguages '(en)'`.

## Window

| Area | Contents | XPC commands |
|---|---|---|
| Livewire network | Active interface or search status; automatic selection (interface receiving Livewire advertisements) or forced Ethernet interface; output advertisement | `status` (5 Hz), `ifaces`, `set_iface`, `set_advertise` |
| Mac inputs | Received channel count (1–16 stereo) or input-device count; default Mac input and button to select OpenLW; matrix rows: discovered sources (ADV), configured unadvertised streams, and manually entered channels; columns: device inputs (one device) or input devices (several devices). Pairs (or devices) are coupled in stereo by default: a click patches the source in stereo, a second click releases it; a surround source takes 8 inputs from the pair. The link button in a pair's header uncouples it: each input then has an L half and an R half (both = L+R), and a source can feed several inputs. The grid scrolls horizontally, the source column stays fixed. Meter and state per pair or device (free, waiting, receiving audio) | `sources`, `config` (2 s), `patch_input`, `unpatch_input`, `remove_input` |
| Mac outputs | Transmitted channel count (1–16 stereo) or output-device count; default Mac output and button to select OpenLW; one row per output pair or output device: meter, channel, advertised name, format (Standard 5 ms, AES67 1 ms, Livestream 0.25 ms), **Transmit** checkbox | `set_device_channels`, `patch_output`, `unpatch_output` |

**Meters:** peak meters, polled with `meters` at 30 Hz (daemon peaks held over 50 ms, refreshed every 10 ms) and drawn at the display rate with peak-meter ballistics: instant rise, fall of 20 dB in 1.7 s (IEC 60268-10 type I return). Against an older daemon without `meters`, peaks come with `status` at 5 Hz. The Linux and Windows apps behave the same.

Reducing the channel count removes out-of-range patches after confirmation. The device is recreated, briefly interrupting audio in applications using it.

**Advanced settings** (collapsible section, state persisted):

| Setting | Values | Effect |
|---|---|---|
| Advertised Mac name | Up to 32 characters; empty = computer name | Advertisement `ATRN`, transliterated to ASCII |
| Receive latency | Low, Normal (default), Safe | 6, 12, or 24 ms jitter buffer and 128, 256, or 512-frame plugin input margin: approximately 8, 17, or 35 ms added; 256, 512, or 1024-frame transmit reserve |
| Network priority (DSCP) | EF 46 (default), AF41 34, none 0 | Marks outgoing audio streams |

XPC command: `set_advanced` (`terminal_name`, `latency`, `dscp`); CLI: `lw-daemon ctl set-advanced`.

OpenLW menu > **Uninstall OpenLW…**: runs `/Library/Application Support/OpenLW/uninstall.sh` with administrator privileges (provided by the installer).

**Preview:** the headphone button at the start of a row plays the source on the Mac's default output without patching it; a level meter replaces the provenance indicator. One source at a time; click again to stop. The app receives multicast directly on the session interface (daemon and app share port 5004 through `SO_REUSEPORT`). Stereo playback at 48 kHz through AudioQueue, with a 30 ms buffer; only channels 1 and 2 of a surround source are played. Preview is rejected when the Mac's default output is “OpenLW”, which would send it back to the network.

**Matrix rows:** discovered sources, manually entered channels (persisted between launches), and configured but unadvertised streams. The ✕ button removes a manual or unadvertised row and releases its inputs if patched.

**Audio device** ([ADR 0010](../../docs/adr/0010-macos-device-layouts.md)): one multichannel “OpenLW” duplex device (fixed name, default), or several devices “OpenLW In n” / “OpenLW Out n”, one source each, as wide as the source (1 channel for a mono patch, 2 for stereo or empty, 8 for surround). With several devices, names follow the source by default, for example “OpenLW In - Studio A@Omnia One (ch. 2)” or “OpenLW Out - MAC 1-2 (ch. 4005)”; unchecking the option gives “OpenLW In 2”. Switching layout converts patches (pair n ↔ device n) after confirmation. Uncoupling or coupling a device (mono ↔ stereo), a surround patch on a device, a layout change, or a device-count change recreates the shared region and stops every OpenLW device briefly: the app asks first (“Do not ask again” available). Channels always carry their source name (for example “2 - Studio A L”, or “2 - Studio A (L+R)” in mono), visible in Audio MIDI Setup and applications that display it. Audacity stores devices by name: reselect the device after changing its layout or name.

A surround source occupies eight inputs starting at the clicked pair. Changing a transmitted output's channel stops the old channel before transmitting on the new one. Test sources with no associated outputs (configuration sine generator) are hidden.

Changing patches requires administrator membership (`admin` group): the daemon checks the caller's UID. Daemon errors appear below the header.

## Appearance

Liquid Glass panels (`NSGlassEffectView`) on macOS 26 and later, `NSVisualEffectView` on earlier versions. Vibrant window background (`.sidebar`). System colors and automatic dark mode.

## Test without installation

The daemon can run as a LaunchAgent under another service name. Connect the app with:

```
"macos/app/build/OpenLW.app/Contents/MacOS/OpenLW" --user --service fr.francois-brille.openlw.daemon.dev
```

Simulated test environment on lo0 (one machine):

```
python3 tools/lw/emit_adv.py --iface lo0 --ip 127.0.0.2 --channel 4001 --name "STUDIO-A PGM" --terminal STUDIO-A
build/lw-daemon send --iface lo0 --channel 4001 --level -12
```

Validated in this environment: automatic mode searching; then forced lo0, two channels in each direction (device generation 2), discovered sources, patch 4001 → inputs 3–4, −12 dBFS meter, receiving-audio state, outputs 1–2 transmitted on 4005. Preview of 4001 (`--listen 4001`): received level displayed while the daemon still feeds inputs 3–4.

## Pending work

- Validate on 10.13 (VM or Intel Mac): untested.
- Preview playback through speakers: AudioQueue started without errors, but audible output not checked.
- Actual matrix/list clicks: the XPC path matches the validated `lw-daemon ctl` path; mouse interaction remains to be checked on screen.
- Developer ID signing and notarization.
