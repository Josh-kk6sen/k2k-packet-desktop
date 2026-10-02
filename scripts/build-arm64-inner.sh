#!/bin/bash
set -euo pipefail

# ARM64 cross-build script for K2K Packet Desktop
# Runs inside an arm64v8/ubuntu:24.04 container

export DEBIAN_FRONTEND=noninteractive

echo "=== Step 1: System dependencies ==="
apt-get update
apt-get install -y \
    build-essential curl wget file \
    libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libssl-dev pkg-config patchelf

echo "=== Step 2: Install Rust ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"

echo "=== Step 3: Install Node.js 22 ==="
curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
apt-get install -y nodejs

echo "=== Step 4: Build frontend ==="
cd /build
npm install
npm run build

echo "=== Step 5: Build Rust backend ==="
cd /build/src-tauri
cargo build --release

echo "=== Step 6: Build .deb package ==="
cargo tauri build --bundles deb

echo "=== Build complete ==="
ls -la /build/src-tauri/target/release/k2k-packet-desktop
ls -la /build/src-tauri/target/release/bundle/deb/ 2>/dev/null || echo "No deb found yet"
