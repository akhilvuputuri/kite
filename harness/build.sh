#!/usr/bin/env bash
# Builds the release binary the way releases ship it and prints its path:
# static musl on Linux, native on macOS. Paths are remapped so the bytes
# do not depend on where the source or the cargo home live.
set -euo pipefail
cd "$(dirname "$0")/.."
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64) target=aarch64-unknown-linux-musl ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *) echo "unsupported build host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
if [ "$(uname -s)" = Linux ] && ! command -v musl-gcc >/dev/null; then
  sudo apt-get update -qq >&2 && sudo apt-get install -y -qq musl-tools >&2
fi
rustup target add "$target" >&2
export RUSTFLAGS="--remap-path-prefix=$PWD=/kite --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
export SOURCE_DATE_EPOCH=0
cargo build --locked --release --target "$target" >&2
echo "$PWD/target/$target/release/kite"
