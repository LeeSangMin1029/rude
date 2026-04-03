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

export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo &>/dev/null; then
    if ! command -v rustup &>/dev/null; then
        echo "Rust not found. Installing via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        source "$HOME/.cargo/env"
    fi
fi

echo "Installing nightly toolchain..."
rustup toolchain install nightly 2>&1 | tail -1
# rustc-dev is only needed if analyzing rustc_private projects (like mir-callgraph itself)
# Install it if we're in the rude repo (has tools/mir-callgraph)
if [ -f "tools/mir-callgraph/Cargo.toml" ]; then
    rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly 2>&1 | tail -1
fi

# --- Download helper ---
download() {
    local name="$1" dest="$2"
    if command -v gh &>/dev/null; then
        gh release download --repo "$REPO" --pattern "${name}" --dir /tmp --clobber 2>/dev/null \
            && mv "/tmp/${name}" "$dest" && chmod +x "$dest" 2>/dev/null && return 0
    fi
    curl -fsSLo "$dest" "https://github.com/${REPO}/releases/latest/download/${name}" 2>/dev/null \
        && chmod +x "$dest" 2>/dev/null && return 0
    return 1
}

# --- rude ---
RUDE_DEST="${HOME}/.cargo/bin/rude${EXT}"
mkdir -p "$(dirname "$RUDE_DEST")"
if download "rude${EXT}" "$RUDE_DEST"; then
    echo "rude: downloaded"
elif [ -f "crates/rude/Cargo.toml" ]; then
    echo "rude: building from source..."
    cargo install --path crates/rude --force 2>&1 | tail -1
else
    echo "rude: building from git..."
    cargo install --git "https://github.com/${REPO}.git" rude --force 2>&1 | tail -1
fi

# --- mir-callgraph ---
mkdir -p "$RUDE_HOME/bin"
MIR_DEST="$RUDE_HOME/bin/mir-callgraph${EXT}"
if download "mir-callgraph${EXT}" "$MIR_DEST"; then
    echo "mir-callgraph: downloaded"
elif [ -f "tools/mir-callgraph/Cargo.toml" ]; then
    echo "mir-callgraph: building from source..."
    (cd tools/mir-callgraph && RUSTUP_TOOLCHAIN=nightly cargo build --release 2>&1 | tail -1)
    cp "tools/mir-callgraph/target/release/mir-callgraph${EXT}" "$MIR_DEST"
else
    echo "mir-callgraph: will auto-build on first 'rude add'"
fi

rustup run nightly rustc --version 2>/dev/null | tr -d '\n' > "$RUDE_HOME/bin/.nightly-version"

# --- go-callgraph (optional) ---
GO_DEST="$RUDE_HOME/bin/go-callgraph${EXT}"
find_go() {
    command -v go &>/dev/null && return 0
    for d in "/c/go/bin" "/c/Program Files/Go/bin" "/usr/local/go/bin" "$HOME/go/bin" "$HOME/.local/go/bin"; do
        [ -x "$d/go${EXT}" ] && export PATH="$d:$PATH" && return 0
    done
    return 1
}
if [ -f "tools/go-callgraph/main.go" ] && find_go; then
    echo "go-callgraph: building from source... ($(go version | head -c 20))"
    (cd tools/go-callgraph && go build -o "go-callgraph${EXT}" . 2>&1 | tail -1)
    cp "tools/go-callgraph/go-callgraph${EXT}" "$GO_DEST"
    echo "go-callgraph: installed"
else
    echo "go-callgraph: skipped (Go SDK not found or not in rude repo)"
fi

# --- ts-callgraph (optional) ---
TS_DIR="$RUDE_HOME/lib/ts-callgraph"
if [ -f "tools/ts-callgraph/package.json" ] && command -v node &>/dev/null; then
    echo "ts-callgraph: installing..."
    mkdir -p "$TS_DIR"
    cp -r tools/ts-callgraph/package.json tools/ts-callgraph/tsconfig.json "$TS_DIR/"
    cp -r tools/ts-callgraph/src "$TS_DIR/"
    (cd "$TS_DIR" && npm install --silent 2>&1 | tail -1 && npx tsc 2>&1 | tail -1)
    echo "ts-callgraph: installed"
else
    echo "ts-callgraph: skipped (Node.js not found or not in rude repo)"
fi

echo ""
echo "=== Done ==="
echo "  rude:           $(which rude 2>/dev/null || echo "$RUDE_DEST")"
echo "  mir-callgraph:  $MIR_DEST"
[ -f "$GO_DEST" ] && echo "  go-callgraph:   $GO_DEST"
[ -d "$TS_DIR/dist" ] && echo "  ts-callgraph:   $TS_DIR/dist/index.js"
echo ""
echo "  rude add .       # Index project (Rust/Go/TS auto-detect)"
echo "  rude ctx fn -s   # Function context"
echo "  rude dead        # Dead code"
