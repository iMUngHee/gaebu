#!/usr/bin/env bash
# Build gaebu in release mode and install it into ~/.local/bin.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${GAEBU_BIN_DIR:-$HOME/.local/bin}"
cargo build --release --locked --manifest-path "$root/Cargo.toml"
mkdir -p "$bin"
exe="gaebu"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe="gaebu.exe" ;; esac
cp "$root/target/release/$exe" "$bin/$exe"
echo "installed $bin/$exe"
"$bin/$exe" --version
