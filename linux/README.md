# OpenLW for Linux

x86_64 and ARM64, distributions with PipeWire and systemd (Ubuntu 22.04, Debian 12, Fedora 38 and later). Audio device: PipeWire nodes served by the daemon ([ADR 0009](../docs/adr/0009-audio-linux.md)).

| Component | Directory | Status |
|---|---|---|
| Network service | `daemon/`, unit `linux/packaging/openlw.service` | Implemented; validation on a real machine pending |
| PipeWire nodes | `daemon/lw-pw` | Planned |
| OpenLW app | `linux/app` (GTK4, libadwaita, Rust) | Planned |
| Packages | `linux/packaging` (`.deb`, `.rpm`) | Planned |

## Development service installation

```sh
(cd daemon && cargo build --release)
install -D daemon/target/release/lw-daemon ~/.local/bin/lw-daemon
mkdir -p ~/.config/systemd/user
sed "s#/usr/bin/lw-daemon#$HOME/.local/bin/lw-daemon#" linux/packaging/openlw.service \
    > ~/.config/systemd/user/openlw.service
systemctl --user daemon-reload
systemctl --user enable --now openlw.service
~/.local/bin/lw-daemon ctl status
journalctl --user -u openlw -f
```

- Configuration: `~/.config/openlw/lw-daemon.json` (created on first startup).
- Control: `$XDG_RUNTIME_DIR/openlw/control.sock`, restricted to the service user.
- Real-time scheduling: membership in the `pipewire` or `audio` group with an `rtprio` limit (distribution PipeWire packages); otherwise the log reports that real-time scheduling was denied.
