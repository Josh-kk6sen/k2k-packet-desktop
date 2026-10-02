# syntax=docker/dockerfile:1
FROM arm64v8/ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Install system dependencies for Tauri + Rust + Node
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    wget \
    file \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libssl-dev \
    pkg-config \
    patchelf \
    && rm -rf /var/lib/apt/lists/*

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# Install Node.js 22 LTS
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy package files first (caching)
COPY package.json package-lock.json* ./
RUN npm install 2>&1

# Copy frontend source
COPY index.html vite.config.js ./
COPY src/ ./src/

# Build frontend
RUN npm run build 2>&1

# Copy Rust source
COPY src-tauri/ ./src-tauri/

# Build the Rust backend in release mode and create .deb bundle
RUN cd src-tauri && \
    cargo build --release --target-dir /build/target 2>&1 && \
    cargo tauri build --bundles deb --target-dir /build/target 2>&1 || \
    echo "DEBUG: checking target layout..." && ls -la /build/target/release/bundle/deb/ 2>/dev/null || true

# Final check
RUN echo "=== Build artifacts ===" && \
    ls -la /build/target/release/k2k-packet-desktop 2>/dev/null && \
    ls -la /build/target/release/bundle/deb/ 2>/dev/null || true
