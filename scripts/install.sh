#!/usr/bin/env bash
# Build gaebu in release mode and install it into ~/.local/bin.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${GAEBU_BIN_DIR:-$HOME/.local/bin}"
cargo build --release --locked --manifest-path "$root/Cargo.toml"
mkdir -p "$bin"
exe="gaebu"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe="gaebu.exe" ;; esac
# Replace by rename, never in place: macOS caches a binary's code signature per
# inode and kills the upgraded binary (SIGKILL) when cp rewrites the old file.
tmp="$bin/.$exe.new.$$"
trap 'rm -f "$tmp"' EXIT
cp "$root/target/release/$exe" "$tmp"
mv -f "$tmp" "$bin/$exe"
echo "installed $bin/$exe"
"$bin/$exe" --version
