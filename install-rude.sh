#!/bin/bash
set -e
echo "[v-code] Installing..."

# Detect OS
OS=$(uname -s)
case "$OS" in
    MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;;
    *) EXT="" ;;
esac

# Install to PATH
REPO="LeeSangMin1029/hnsw-model2vec"
DEST="${HOME}/.cargo/bin/v-code${EXT}"
mkdir -p "$(dirname "$DEST")"

if command -v gh &>/dev/null; then
    echo "[v-code] Downloading via gh..."
    gh release download --repo "$REPO" --pattern "v-code${EXT}" --dir /tmp --clobber 2>/dev/null
    mv "/tmp/v-code${EXT}" "$DEST"
else
    echo "[v-code] Downloading via curl..."
    curl -sLo "$DEST" "https://github.com/${REPO}/releases/latest/download/v-code${EXT}"
fi

chmod +x "$DEST" 2>/dev/null || true
echo "[v-code] Installed to $DEST"

# Install nightly
echo "[v-code] Installing nightly rustc..."
if command -v rustup &>/dev/null; then
    rustup toolchain install nightly
    rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
else
    echo "[v-code] rustup not found. Install Rust first: https://rustup.rs"
    exit 1
fi

echo "[v-code] Done! Run: v-code add .code.db ."
