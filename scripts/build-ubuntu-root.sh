#!/usr/bin/env bash
# Run inside an isolated Ubuntu 22.04 root with this checkout at /work.
set -euo pipefail
test "${ESL_DISPOSABLE_ROOT:-}" = 1 || { echo 'Use only with ESL_DISPOSABLE_ROOT=1 inside a disposable Ubuntu root' >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive
printf '%s\n' 'deb [arch=amd64] http://archive.ubuntu.com/ubuntu jammy main universe' \
    'deb [arch=amd64] http://archive.ubuntu.com/ubuntu jammy-updates main universe' \
    'deb [arch=amd64] http://security.ubuntu.com/ubuntu jammy-security main universe' > /etc/apt/sources.list
apt-get update -qq
apt-get install -y --no-install-recommends build-essential curl pkg-config libudev-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libx11-dev libxi-dev libxrandr-dev libxcursor-dev libegl1-mesa-dev libgl1-mesa-dev libwayland-dev \
    python3 python3-gi xz-utils ca-certificates git dpkg-dev dbus systemd
if ! test -x /root/.cargo/bin/cargo; then
    curl --fail --silent --show-error https://sh.rustup.rs -o /tmp/rustup-init.sh
    sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain 1.98.1
fi
export PATH=/root/.cargo/bin:$PATH
rustup component add clippy rustfmt
if ! command -v cargo-deb >/dev/null; then cargo install cargo-deb --version 3.8.0 --locked; fi
cd /work
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
bash scripts/build-linux.sh x86_64-unknown-linux-gnu
dbus-run-session -- python3 scripts/test-tray-startup.py dist/easy-switch-link-0.3.4-linux-amd64
