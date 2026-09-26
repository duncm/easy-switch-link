#!/usr/bin/env bash
# Cross-package ARM64 against Ubuntu 22.04; native tests run separately on ARM64.
set -euo pipefail
test "${ESL_DISPOSABLE_ROOT:-}" = 1 || { echo 'Use only with ESL_DISPOSABLE_ROOT=1 inside a disposable Ubuntu root' >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive PATH=/root/.cargo/bin:$PATH
dpkg --add-architecture arm64
printf '%s\n' 'deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy main universe' \
    'deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy-updates main universe' \
    'deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy-security main universe' > /etc/apt/sources.list.d/arm64.list
apt-get update -qq
apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu g++-aarch64-linux-gnu \
    libudev-dev:arm64 libxkbcommon-dev:arm64 libxkbcommon-x11-dev:arm64 libx11-dev:arm64 \
    libxi-dev:arm64 libxrandr-dev:arm64 libxcursor-dev:arm64 libegl1-mesa-dev:arm64 libgl1-mesa-dev:arm64 libwayland-dev:arm64
cd /work
rustup target add aarch64-unknown-linux-gnu
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
bash scripts/build-linux.sh aarch64-unknown-linux-gnu
