#!/bin/bash
set -euo pipefail

# Runs the ARM64 build inside a Docker container with QEMU emulation
# Uses a clean temp dir so we don't pollute host node_modules/target
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${PROJECT_DIR}/downloads/K2KPacketDesktop"
CONTAINER_NAME="k2k-packet-arm64-builder"

# Clean up any previous container
docker rm -f "${CONTAINER_NAME}" 2>/dev/null || true

echo "=== Starting ARM64 build container ==="
echo "This will take a while (QEMU emulation is slow). Grab a coffee."

# Run the container with the project source mounted
# The build script will copy sources to /build inside the container
# to avoid contaminating the host's node_modules/target
docker run --name "${CONTAINER_NAME}" \
    --platform linux/arm64 \
    -v "${PROJECT_DIR}:/src:ro" \
    -v "${PROJECT_DIR}/scripts/build-arm64.sh:/setup.sh:ro" \
    arm64v8/ubuntu:24.04 \
    bash -c '
set -euo pipefail

echo "=== Step 1: System dependencies ==="
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y \
    build-essential curl wget file \
    libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev \
    libssl-dev pkg-config patchelf

echo "=== Step 2: Install Rust ==="
curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"

echo "=== Step 3: Install Node.js 22 ==="
curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
apt-get install -y nodejs
rm -rf /var/lib/apt/lists/*

echo "=== Step 4: Copy source to clean workspace ==="
mkdir -p /workspace
cp -a /src/. /workspace/
cd /workspace

# Remove any host x86 artifacts that would break the build
rm -rf node_modules target dist

echo "=== Step 5: Build frontend ==="
npm install
npm run build

echo "=== Step 6: Build Rust backend ==="
cd src-tauri
cargo build --release
cd /workspace

echo "=== Step 7: Build .deb bundle ==="
npx tauri build --bundles deb
cd /workspace

echo "=== Build complete ==="
echo "Binary:"
file src-tauri/target/release/k2k-packet-desktop
ls -lh src-tauri/target/release/k2k-packet-desktop
echo ""
echo "Deb:"
ls -lh src-tauri/target/release/bundle/deb/ 2>/dev/null || echo "No deb found"
echo ""
echo "All artifacts in /workspace/src-tauri/target/release/"
ls -lh src-tauri/target/release/k2k-packet-desktop
ls src-tauri/target/release/bundle/deb/ 2>/dev/null
' 2>&1

BUILD_EXIT=$?
echo ""
echo "=== Container finished with exit code: ${BUILD_EXIT} ==="

if [ ${BUILD_EXIT} -ne 0 ]; then
    echo "Build FAILED - check output above"
    exit ${BUILD_EXIT}
fi

echo ""
echo "=== Extracting artifacts from container ==="
mkdir -p "${OUTPUT_DIR}"

# Copy binary
docker cp "${CONTAINER_NAME}:/workspace/src-tauri/target/release/k2k-packet-desktop" "${OUTPUT_DIR}/k2k-packet-desktop-arm64"
echo "Copied ARM64 binary"

# Copy .deb
docker cp "${CONTAINER_NAME}:/workspace/src-tauri/target/release/bundle/deb/." "${OUTPUT_DIR}/" 2>/dev/null || true
# Rename deb to include arm64
for DEB in "${OUTPUT_DIR}"/*.deb; do
    if [ -f "${DEB}" ]; then
        BASENAME=$(basename "${DEB}")
        DIRNAME=$(dirname "${DEB}")
        mv "${DEB}" "${DIRNAME}/k2k-packet-desktop_0.2.1_arm64.deb"
        echo "Copied and renamed: ${BASENAME} -> k2k-packet-desktop_0.2.1_arm64.deb"
    fi
done

echo ""
echo "=== Files ready in ${OUTPUT_DIR} ==="
ls -lh "${OUTPUT_DIR}"/k2k-packet-desktop-arm64*
ls -lh "${OUTPUT_DIR}"/*.deb 2>/dev/null || true

# Clean up
docker rm -f "${CONTAINER_NAME}" 2>/dev/null || true
echo "=== Done ==="
