#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
target="${1:-x86_64-unknown-linux-gnu}"
case "$target" in
  x86_64-unknown-linux-gnu) arch=amd64 ;;
  aarch64-unknown-linux-gnu) arch=arm64 ;;
  *) echo "Unsupported target: $target" >&2; exit 1 ;;
esac
cargo build --locked --release -p easy-switch-link --target "$target"
mkdir -p dist
python3 scripts/package-gnome.py
cp "${CARGO_TARGET_DIR:-target}/$target/release/easy-switch-link" "dist/easy-switch-link-0.3.0-linux-$arch"
cargo deb --no-build --no-strip -p easy-switch-link --target "$target" --output "dist/easy-switch-link_0.3.0_${arch}.deb"
(cd dist && sha256sum "easy-switch-link-0.3.0-linux-$arch" "easy-switch-link_0.3.0_${arch}.deb" easy-switch-link-0.3.0-gnome.shell-extension.zip > "SHA256SUMS-linux-$arch.txt")
