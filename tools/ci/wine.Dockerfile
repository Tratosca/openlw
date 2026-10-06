# Windows tests without Windows hardware: Rust (x86_64-pc-windows-gnu target), mingw-w64, Wine.
# Wine 10+ (bcryptprimitives.dll required by Rust standard library).
# Covers C layer and daemon tests; does not replace real Windows testing (MMCSS,
# tokens, ACLs only emulated by Wine).
#
#   docker build --platform linux/amd64 -t openlw-wine -f tools/ci/wine.Dockerfile tools/ci
#   docker run --rm --platform linux/amd64 -v "$PWD":/src -w /src/daemon openlw-wine cargo test --target x86_64-pc-windows-gnu
FROM rust:1-trixie
RUN dpkg --add-architecture i386 \
    && apt-get update \
    && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 wine wine64 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-pc-windows-gnu
ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine \
    WINEDEBUG=-all \
    OPENLW_COARSE_TIMERS=1 \
    WINEPREFIX=/wine
RUN wineboot --init 2>/dev/null || true
