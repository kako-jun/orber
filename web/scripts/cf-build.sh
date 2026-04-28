#!/usr/bin/env bash
# Cloudflare Pages build script for the orber web frontend.
#
# CF Pages のデフォルトビルド環境には cargo が無いため、ここで rustup と
# wasm-pack を都度セットアップする。手元の dev では `npm run build` を直接
# 叩く（このスクリプトは CF からのみ呼ばれる）。
set -euo pipefail

# 1. Rust toolchain
if ! command -v cargo >/dev/null 2>&1; then
  if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "[cf-build] installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

echo "[cf-build] cargo: $(cargo --version)"
rustup target add wasm32-unknown-unknown

# 2. wasm-pack
if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "[cf-build] installing wasm-pack..."
  cargo install wasm-pack --locked
fi
echo "[cf-build] wasm-pack: $(wasm-pack --version)"

# 3. wasm + astro build
echo "[cf-build] running npm run build..."
npm run build
