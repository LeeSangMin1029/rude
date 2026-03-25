#!/bin/bash
set -e
echo "[rude] Installing..."

# Detect OS
OS=$(uname -s)
case "$OS" in
    MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;;
    *) EXT="" ;;
esac

# Install to PATH
REPO="LeeSangMin1029/rude"
DEST="${HOME}/.cargo/bin/rude${EXT}"
mkdir -p "$(dirname "$DEST")"

if command -v gh &>/dev/null; then
    echo "[rude] Downloading via gh..."
    gh release download --repo "$REPO" --pattern "rude${EXT}" --dir /tmp --clobber 2>/dev/null
    mv "/tmp/rude${EXT}" "$DEST"
else
    echo "[rude] Downloading via curl..."
    curl -sLo "$DEST" "https://github.com/${REPO}/releases/latest/download/rude${EXT}"
fi

chmod +x "$DEST" 2>/dev/null || true
echo "[rude] Installed to $DEST"

# Install nightly (required for mir-callgraph)
echo "[rude] Installing nightly rustc..."
if command -v rustup &>/dev/null; then
    rustup toolchain install nightly
    rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
else
    echo "[rude] rustup not found. Install Rust first: https://rustup.rs"
    exit 1
fi

echo "[rude] Done! Run: rude add .code.db ."
