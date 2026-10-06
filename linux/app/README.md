# OpenLW app for Linux

GTK 4 and libadwaita configuration app (Rust, gtk4-rs and libadwaita-rs), x86_64 and ARM64. Same features and layout as the macOS app ([macos/app](../../macos/app/README.md)) and the Windows app ([windows/app](../../windows/app/README.md)):

| Section | Contents |
|---|---|
| Réseau Livewire | Connection state, interface (automatic or forced), source advertisement |
| Périphérique audio | State of the PipeWire nodes “OpenLW Out” and “OpenLW In” published by the service |
| Entrées | Patch matrix (discovered, manual and unadvertised sources × input pairs), per-pair meters and state, headphone preview |
| Sorties | One row per output pair: meter, Livewire channel, advertised name, format, transmission |
| Réglages avancés | Advertised name, receive latency, DSCP |

User-facing strings are in French, like the other apps. Application ID: `fr.francois_brille.openlw` (GTK and D-Bus IDs do not accept the hyphen of `francois-brille`).

Floor: GTK 4.12 and libadwaita 1.4 (Ubuntu 24.04, Debian 13, Fedora 39 and later). On Debian 12, only the service is supported (package `openlw-daemon`).

## Service connection

Unix socket `$XDG_RUNTIME_DIR/openlw/control.sock`, one JSON request per line ([ADR 0007](../../docs/adr/0007-canal-de-controle.md)). The app runs as the user who owns the service: no privilege escalation. When the service does not answer, a banner offers to start it (`systemctl --user enable --now openlw.service`).

## Preview

The headphone button joins the source multicast group on the Livewire interface (shared port 5004, socket bound to the group address), decodes RTP L24/L16 and plays the first two channels through a PipeWire playback stream: 30 ms buffer before playback, excess above 200 ms discarded.

The preview never plays to “OpenLW Out”, which would send it back to the network: it targets the default output, or the first other output when the default is “OpenLW Out”. Without any other output, the preview is refused with an explicit message.

App-only preferences (manual channels, advanced section expanded) are stored in `~/.config/openlw/app.json`.

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
