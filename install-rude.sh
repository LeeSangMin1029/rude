#!/usr/bin/env bash
set -euo pipefail

REPO="LeeSangMin1029/rude"
RUDE_HOME="${RUDE_HOME:-$HOME/.rude}"

echo "=== rude installer ==="

OS="$(uname -s)"
case "$OS" in
    MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;;
    *)                     EXT="" ;;
esac

# --- Rust toolchain ---
if ! command -v rustup &>/dev/null; then
    echo "Rust not found. Installing via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source "$HOME/.cargo/env"
fi
echo "rustup: $(rustup --version 2>/dev/null | head -1)"

echo "Installing nightly toolchain..."
rustup toolchain install nightly 2>&1 | tail -1

# --- rude binary ---
install_rude_binary() {
    local dest="${HOME}/.cargo/bin/rude${EXT}"
    mkdir -p "$(dirname "$dest")"
    if command -v gh &>/dev/null; then
        gh release download --repo "$REPO" --pattern "rude${EXT}" --dir /tmp --clobber 2>/dev/null
        mv "/tmp/rude${EXT}" "$dest"
    else
        curl -fsSLo "$dest" "https://github.com/${REPO}/releases/latest/download/rude${EXT}"
    fi
    chmod +x "$dest" 2>/dev/null || true
}

if [ -f "crates/rude/Cargo.toml" ]; then
    echo "Local repo detected — building from source..."
    cargo install --path crates/rude --force 2>&1 | tail -1
else
    echo "Downloading rude binary..."
    if ! install_rude_binary 2>/dev/null; then
        echo "Download failed — building from source..."
        cargo install --git "https://github.com/${REPO}.git" rude --force 2>&1 | tail -1
    fi
fi

# --- mir-callgraph binary ---
mkdir -p "$RUDE_HOME/bin"

install_mir_binary() {
    local dest="$RUDE_HOME/bin/mir-callgraph${EXT}"
    if command -v gh &>/dev/null; then
        gh release download --repo "$REPO" --pattern "mir-callgraph${EXT}" --dir /tmp --clobber 2>/dev/null
        mv "/tmp/mir-callgraph${EXT}" "$dest"
    else
        curl -fsSLo "$dest" "https://github.com/${REPO}/releases/latest/download/mir-callgraph${EXT}"
    fi
    chmod +x "$dest" 2>/dev/null || true
}

if [ -f "tools/mir-callgraph/Cargo.toml" ]; then
    echo "Building mir-callgraph from source (needs rustc-dev)..."
    rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly 2>&1 | tail -1
    (cd tools/mir-callgraph && cargo +nightly build --release 2>&1 | tail -1)
    cp "tools/mir-callgraph/target/release/mir-callgraph${EXT}" "$RUDE_HOME/bin/"
else
    echo "Downloading mir-callgraph binary..."
    if ! install_mir_binary 2>/dev/null; then
        echo "No binary available — will auto-build on first 'rude add'"
    fi
fi

rustup run nightly rustc --version 2>/dev/null | awk '{print $2}' > "$RUDE_HOME/bin/.nightly-version"

echo ""
echo "=== Installation complete ==="
echo "  rude:           $(which rude 2>/dev/null || echo '~/.cargo/bin/rude')"
echo "  mir-callgraph:  $RUDE_HOME/bin/mir-callgraph${EXT}"
echo ""
echo "  rude add .          # Index project"
echo "  rude ctx func -s    # Function context"
echo "  rude dead           # Dead code"
echo "  rude trace a b      # Call path"
