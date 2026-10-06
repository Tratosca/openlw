# OpenLW pour Linux

x86_64 et ARM64, distributions avec PipeWire et systemd (Ubuntu 22.04, Debian 12, Fedora 38 et plus). Périphérique audio : nœuds PipeWire servis par le daemon ([ADR 0009](../docs/adr/0009-audio-linux.md)).

| Composant | Dossier | État |
|---|---|---|
| Service réseau | `daemon/`, unité `linux/packaging/openlw.service` | prêt, à valider sur machine réelle |
| Nœuds PipeWire | `daemon/lw-pw` | à faire |
| App OpenLW | `linux/app` (GTK4, libadwaita, Rust) | à faire |
| Paquets | `linux/packaging` (`.deb`, `.rpm`) | à faire |

## Service, installation de développement

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

- Configuration : `~/.config/openlw/lw-daemon.json` (créée au premier démarrage).
- Contrôle : socket `$XDG_RUNTIME_DIR/openlw/control.sock`, réservée à l'utilisateur du service.
- Temps réel : membre du groupe `pipewire` ou `audio` avec une limite `rtprio` (paquets PipeWire des distributions) ; sinon le journal signale « temps réel refusé ».
