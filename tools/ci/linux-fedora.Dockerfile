# RPM packages (linux/packaging/build-packages.sh rpm): daemon and app built on Fedora 43.
#   docker build --platform linux/arm64 -t openlw-fedora -f tools/ci/linux-fedora.Dockerfile tools/ci
FROM fedora:43
RUN dnf install -y --setopt=install_weak_deps=False \
       gcc clang pkgconf-pkg-config gtk4-devel libadwaita-devel pipewire-devel \
       rpm-build desktop-file-utils appstream \
    && dnf clean all
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && cargo install --locked cargo-generate-rpm \
    && rm -rf /usr/local/cargo/registry
