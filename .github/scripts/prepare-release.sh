#!/usr/bin/env bash
set -euo pipefail
VERSION="$1"

# Bump version in the CLI crate only.
# cuc-lib keeps its own lifecycle: bump it manually + cargo publish only when
# its code actually changes (semantic-release versions the CLI repo-wide).
sed -i "s/^version = .*/version = \"$VERSION\"/" bin/cuc/Cargo.toml

# Refresh Cargo.lock with the new version
cargo check --workspace

# Build both Windows targets (same as the manual release workflow)
cargo build --release --target x86_64-pc-windows-msvc --target i686-pc-windows-msvc

# Stage release assets for @semantic-release/github
mkdir -p dist
cp "target/x86_64-pc-windows-msvc/release/cuc.exe" "dist/cuc-${VERSION}-x64.exe"
cp "target/i686-pc-windows-msvc/release/cuc.exe" "dist/cuc-${VERSION}-x86.exe"
