#!/bin/bash
set -e

REPO="LeeSangMin1029/rude"
RUDE_HOME="${RUDE_HOME:-$HOME/.rude}"

# Detect OS + arch
OS=$(uname -s)
case "$OS" in
    MINGW*|MSYS*|CYGWIN*) PLATFORM="windows"; EXT=".exe" ;;
    Darwin*)               PLATFORM="macos";   EXT="" ;;
    Linux*)                PLATFORM="linux";   EXT="" ;;
    *)                     PLATFORM="unknown";  EXT="" ;;
esac

echo "[rude] Platform: $PLATFORM"

# Ensure rustup + nightly
if ! command -v rustup &>/dev/null; then
    echo "[rude] ERROR: rustup not found. Install Rust first: https://rustup.rs"
    exit 1
fi

# Install/update nightly with required components
echo "[rude] Setting up nightly toolchain..."
rustup toolchain install nightly --component rust-src rustc-dev llvm-tools-preview 2>&1 | tail -1

# Choose install method: binary release or source build
install_binary() {
    local dest="$1"
    echo "[rude] Downloading pre-built binary..."
    if command -v gh &>/dev/null; then
        gh release download --repo "$REPO" --pattern "rude${EXT}" --dir /tmp --clobber 2>/dev/null
        mv "/tmp/rude${EXT}" "$dest"
    else
        curl -fsSLo "$dest" "https://github.com/${REPO}/releases/latest/download/rude${EXT}"
    fi
    chmod +x "$dest" 2>/dev/null || true
}

install_source() {
    echo "[rude] Building from source..."
    if [ -f "crates/rude/Cargo.toml" ]; then
        cargo install --path crates/rude --force
    else
        cargo install --git "https://github.com/${REPO}.git" rude --force
    fi
}

# Determine install destination
if [ -f "crates/rude/Cargo.toml" ]; then
    echo "[rude] Local repo detected — building from source"
    install_source
else
    DEST="${HOME}/.cargo/bin/rude${EXT}"
    mkdir -p "$(dirname "$DEST")"
    if install_binary "$DEST" 2>/dev/null; then
        echo "[rude] Binary installed to $DEST"
    else
        echo "[rude] Binary download failed — falling back to source build"
        install_source
    fi
fi

# mir-callgraph: auto-built on first `rude add`, but pre-build if in repo
if [ -f "tools/mir-callgraph/Cargo.toml" ]; then
    echo "[rude] Building mir-callgraph from local source..."
    mkdir -p "$RUDE_HOME/bin"
    cd tools/mir-callgraph
    cargo +nightly build --release 2>&1 | tail -1
    cp "target/release/mir-callgraph${EXT}" "$RUDE_HOME/bin/"
    # Save nightly version
    rustup run nightly rustc --version | awk '{print $2}' > "$RUDE_HOME/bin/.nightly-version"
    cd ../..
    echo "[rude] mir-callgraph installed to $RUDE_HOME/bin/"
else
    echo "[rude] mir-callgraph will be auto-built on first 'rude add'"
fi

echo ""
echo "[rude] Installation complete!"
echo "  rude:           $(which rude 2>/dev/null || echo '~/.cargo/bin/rude')"
echo "  mir-callgraph:  $RUDE_HOME/bin/mir-callgraph${EXT}"
echo ""
echo "Usage:"
echo "  rude add .          # Index current project (creates .code.db)"
echo "  rude ctx my_func -s # Show function context with source"
echo "  rude dead           # Find dead code"
echo "  rude trace a b      # Call path from a to b"
