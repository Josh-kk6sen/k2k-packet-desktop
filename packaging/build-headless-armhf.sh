#!/bin/bash
# Build the headless (no GUI toolkit) k2k-headless armhf binary + .deb inside
# an emulated armhf Debian container.
#
# Run from the host:  ~/headless-build/run-armhf.sh
#
# Why a container: the x86 builder cannot link armhf, and the LAN Pis are too
# small. This build needs NO GTK/WebKit dev packages at all — that is the whole
# point of the headless target — so it is far quicker than the desktop bundle
# build (minutes, not the 80+ the Tauri armhf build took).
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  build-essential pkg-config curl ca-certificates git dpkg-dev file rsync \
  >/tmp/apt.log 2>&1 || { tail -40 /tmp/apt.log; exit 1; }

if ! command -v cargo >/dev/null; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/tmp/rustup.log 2>&1
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version; cargo --version; uname -m

# Copy the source in: the host tree's src-tauri/target is x86 and must not be
# reused. `dist/` IS needed — the front-end is embedded at compile time.
mkdir -p /build/src
rsync -a --delete \
  --exclude node_modules --exclude src-tauri/target --exclude .git --exclude target \
  /src/ /build/src/
cd /build/src/src-tauri

export CARGO_TARGET_DIR=/build/target
cargo build --release --no-default-features --bin k2k-headless

BIN=/build/target/release/k2k-headless
file "$BIN"
ldd "$BIN"

# ── package ────────────────────────────────────────────────────────────────
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
STAGE=/build/deb
rm -rf "$STAGE"
mkdir -p "$STAGE/usr/bin" \
         "$STAGE/usr/share/doc/k2k-packet-headless" \
         "$STAGE/lib/systemd/system" \
         "$STAGE/DEBIAN"
install -m 0755 "$BIN" "$STAGE/usr/bin/k2k-headless"
install -m 0644 /packaging/k2k-headless.service "$STAGE/lib/systemd/system/"
install -m 0644 /packaging/README.headless "$STAGE/usr/share/doc/k2k-packet-headless/README"

SIZE_KB=$(du -sk "$STAGE/usr" "$STAGE/lib" | awk '{s+=$1} END {print s}')
cat > "$STAGE/DEBIAN/control" <<EOF
Package: k2k-packet-headless
Version: $VERSION
Architecture: armhf
Installed-Size: $SIZE_KB
Maintainer: K2KDesktop
Section: hamradio
Priority: optional
Depends: libc6, libgcc-s1
Description: K2K Packet Desktop - browser-served backend (no GUI toolkit)
 The full K2K packet terminal: the same AX.25/KISS/AGW engine, settings,
 message board and logs as the desktop app, served to a browser over HTTP
 with no window, no GTK and no WebKitGTK. Runs on a Raspberry Pi (32-bit)
 alongside Direwolf and is driven from any machine on the LAN.
EOF

dpkg-deb --root-owner-group -Zxz -b "$STAGE" "/out/k2k-headless_${VERSION}_armhf.deb"
ls -lh "/out/k2k-headless_${VERSION}_armhf.deb"
echo "EXIT=0"
