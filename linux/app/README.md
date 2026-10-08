# OpenLW app for Linux

GTK 4 and libadwaita configuration app (Rust, gtk4-rs and libadwaita-rs), x86_64 and ARM64. Same features and layout as the macOS app ([macos/app](../../macos/app/README.md)) and the Windows app ([windows/app](../../windows/app/README.md)):

| Section | Contents |
|---|---|
| Livewire network | Connection state, interface (automatic or forced), source advertisement |
| Audio device | Layout: two multichannel nodes “OpenLW In” and “OpenLW Out”, or one node per source, “OpenLW In n” and “OpenLW Out n” (optionally named after their source); state of the PipeWire nodes |
| Inputs | Patch matrix (discovered, manual and unadvertised sources × input pairs or input devices): stereo in one click on a coupled pair or device, left and right sides per input once uncoupled, surround on 8 channels; per-pair or per-device meters and state, headphone preview |
| Outputs | One row per output pair or output device: meter, Livewire channel, advertised name, format, transmission |
| Advanced settings | Advertised name, receive latency, DSCP |

## Input grid

Input pairs (two devices: inputs 2n-1 and 2n of “OpenLW In”) and input devices (several devices: “OpenLW In n”) are coupled in stereo by default. On a coupled group, a click patches the source in stereo (a surround source: 8 inputs from the pair start, or an 8-channel device), and a click on a patched group releases it. The link button in the group header uncouples it (`set_coupling`): each input then shows two halves, L on top and R below (G and D in French); a click toggles that side of the source on that input, both sides give L+R. The same source can feed several inputs, and stereo on inputs 2-3 is possible. On uncoupled columns, a surround row still patches as a whole. Patches go to the service as crosspoints (`patch_input` with `taps`, a tap without stream channels releases its input); the configuration returns them as `taps` and `uncoupled_inputs`.

Coupling a pair fed by several sources keeps the source of its first input in stereo and releases the others: the app asks first. In the several-devices layout, an uncoupled device is mono (1 channel), a coupled one stereo (2, or 8 with a surround source), and a surround source needs a coupled device. Coupling, uncoupling, and a patch that changes a device's width recreate the shared region: the app warns first, as all OpenLW nodes stop for a moment (“Do not ask again” is stored in the app preferences).

The source columns (preview, channel, name, origin) stay on the left; with many inputs (16 pairs: 32 columns), the cells scroll horizontally and the window keeps its width.

## Languages

The interface is in English, or in French when the system language is French (`LC_ALL`, then `LC_MESSAGES`, then `LANG`, as gettext reads them). Source strings stay in the code, wrapped in `tr("…")` or `trf("…", …)` (named placeholders `{name}`); the French catalog [po/fr.po](po/fr.po) uses the gettext syntax and is embedded in the binary at build time: no `.mo` file to install, no gettext dependency. Messages of the service and the channel and device names it publishes stay in English. `cargo test` checks that the catalog parses, that every translation keeps its placeholders and French spacing, and that every `tr` string of the code is translated. French follows the Microsoft French Style Guide (formal “vous”, U+00A0 before `: ; ! ?` and inside « »); left and right are G and D in French texts.

Application ID: `fr.francois_brille.openlw` (GTK and D-Bus IDs do not accept the hyphen of `francois-brille`).

Floor: GTK 4.12 and libadwaita 1.4 (Ubuntu 24.04, Debian 13, Fedora 39 and later). On Debian 12, only the service is supported (package `openlw-daemon`).

## Service connection

Unix socket `$XDG_RUNTIME_DIR/openlw/control.sock`, one JSON request per line. The app runs as the user who owns the service: no privilege escalation. When the service does not answer, a banner offers to start it (`systemctl --user enable --now openlw.service`).

## Preview

The headphone button joins the source multicast group on the Livewire interface (shared port 5004, socket bound to the group address), decodes RTP L24/L16 and plays the first two channels through a PipeWire playback stream: 30 ms buffer before playback, excess above 200 ms discarded.

The preview never plays to an OpenLW output (“OpenLW Out”, or “OpenLW Out n”: nodes `openlw_out` and `openlw_out_n`), which would send it back to the network: it targets the default output, or the first other output when the default is an OpenLW output. Without any other output, the preview is refused with an explicit message.

App-only preferences (manual channels, advanced section expanded, dismissed width warning) are stored in `~/.config/openlw/app.json`.

## Build

Prerequisites: Rust 1.92 or later, a C compiler, `pkg-config`, and the development files of GTK 4, libadwaita and PipeWire (`sudo apt install libgtk-4-dev libadwaita-1-dev libpipewire-0.3-dev libspa-0.2-dev clang pkg-config` on Ubuntu 24.04).

```sh
cd linux/app
cargo build --release      # target/release/openlw
cargo test
```

Without a Linux desktop, `tools/ci/preview-gtk.sh` runs the service, PipeWire, two mock Livewire sources and the app (GTK Broadway backend) in containers; open `http://localhost:8085`.

## Dependencies

- gtk4-rs, libadwaita-rs (MIT)
- pipewire-rs (MIT)
- socket2, serde, serde_json (MIT or Apache-2.0)
