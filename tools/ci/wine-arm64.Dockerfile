# Tests Windows ARM64 sans machine Windows, sans émulation sur un hôte arm64 (Apple Silicon,
# Raspberry Pi, runner Linux arm64) : Rust (aarch64-pc-windows-gnullvm), llvm-mingw et Wine natif.
#
#   docker build --platform linux/arm64 -t openlw-wine-arm64 -f tools/ci/wine-arm64.Dockerfile tools/ci
FROM rust:1-trixie
ARG LLVM_MINGW=20260922
RUN apt-get update \
    && apt-get install -y --no-install-recommends wine wine64 xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://github.com/mstorsjo/llvm-mingw/releases/download/${LLVM_MINGW}/llvm-mingw-${LLVM_MINGW}-ucrt-ubuntu-22.04-aarch64.tar.xz" \
       | tar -xJ -C /opt \
    && mv /opt/llvm-mingw-* /opt/llvm-mingw \
    && rustup target add aarch64-pc-windows-gnullvm
ENV PATH=/opt/llvm-mingw/bin:$PATH \
    CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
    AR_aarch64_pc_windows_gnullvm=llvm-ar \
    CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
    CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_RUNNER=wine \
    WINEDEBUG=-all \
    OPENLW_COARSE_TIMERS=1 \
    WINEPATH=Z:\\opt\\llvm-mingw\\aarch64-w64-mingw32\\bin \
    WINEPREFIX=/wine
RUN wineboot --init 2>/dev/null || true
