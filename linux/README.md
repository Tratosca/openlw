# OpenLW for Linux

x86_64 and ARM64, distributions with PipeWire 0.3.49 or later and systemd (Ubuntu 24.04, Debian 12, Fedora 38 and later; Ubuntu 22.04 ships PipeWire 0.3.48 and is not supported). Audio device: PipeWire nodes served by the daemon ([ADR 0009](../docs/adr/0009-audio-linux.md)).

| Component | Directory | Status |
|---|---|---|
| Network service | `daemon/`, unit `linux/packaging/openlw.service` | Implemented; validation on a real machine pending |
| PipeWire nodes | `daemon/lw-pw` (“OpenLW In” source, “OpenLW Out” sink) | Implemented; container test (`tools/ci/test-pipewire.sh`), desktop session pending |
| OpenLW app | `linux/app` (GTK4, libadwaita, Rust; [README](app/README.md)) | Implemented; container test with the Broadway backend (`tools/ci/preview-gtk.sh`), desktop session pending |
| Packages | `linux/packaging` (`.deb`, `.rpm`) | Implemented; installation on a real machine pending |

## Packages

Two packages, built in containers by `linux/packaging/build-packages.sh [deb|rpm|all]` (`PLATFORM=linux/amd64` for x86_64; output in `linux/packaging/dist/`):

| Package | Contents | Built on | Installs on |
|---|---|---|---|
| `openlw-daemon` | `/usr/bin/lw-daemon`, user unit `/usr/lib/systemd/user/openlw.service` | Debian 12 (`.deb`), Fedora 43 (`.rpm`) | Debian 12, Ubuntu 24.04 and later; Fedora 43 and later |
| `openlw` | App `/usr/bin/openlw`, `.desktop` entry, AppStream metadata, icon; depends on `openlw-daemon` | Ubuntu 24.04 (`.deb`), Fedora 43 (`.rpm`) | Ubuntu 24.04, Debian 13 and later; Fedora 43 and later |

Installing `openlw-daemon` enables the user service for every user (`systemctl --global enable`), starting at their next login. In an already open session, the app offers to start it, or:

```sh
systemctl --user enable --now openlw.service
```

Removal disables the unit for future logins; running instances stop at logout.

## Development service installation

Build prerequisites: Rust, a C compiler, and the PipeWire development files (`sudo apt install libpipewire-0.3-dev libspa-0.2-dev clang pkg-config` on Debian and Ubuntu). Without them, `cargo build --release --no-default-features` builds a daemon without PipeWire nodes.

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
