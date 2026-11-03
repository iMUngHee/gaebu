# Build gaebu in release mode and install it into ~/.local/bin.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$bin = if ($env:GAEBU_BIN_DIR) { $env:GAEBU_BIN_DIR } else { Join-Path $HOME '.local\bin' }
cargo build --release --locked --manifest-path (Join-Path $root 'Cargo.toml')
New-Item -ItemType Directory -Force -Path $bin | Out-Null
Copy-Item (Join-Path $root 'target\release\gaebu.exe') (Join-Path $bin 'gaebu.exe') -Force
Write-Output "installed $(Join-Path $bin 'gaebu.exe')"
& (Join-Path $bin 'gaebu.exe') --version
