#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="$SCRIPT_DIR/rust-core"
BIN_DIR="$HOME/.codeseek/bin"
BIN_PATH="$BIN_DIR/codeseek"

echo "==> [1/2] Building TypeScript wrapper..."
cd "$SCRIPT_DIR"
if [ -f "node_modules/.package-lock.json" ] || [ -d "node_modules/typescript" ]; then
    npx tsc 2>/dev/null && echo "    dist/ ready" || echo "    (tsc skipped)"
else
    echo "    (no node_modules, skipping TS build)"
fi

echo ""
echo "==> [2/2] Building Rust binary..."
cd "$RUST_DIR"

MODE="${1:-}"
if [ "$MODE" = "--release" ] || [ "$MODE" = "-r" ]; then
    echo "    Building release..."
    cargo build --release
    RUST_BIN="$RUST_DIR/target/release/codeseek"
else
    echo "    Building debug..."
    cargo build
    RUST_BIN="$RUST_DIR/target/debug/codeseek"
fi

echo ""
echo "==> Installing to $BIN_PATH"
mkdir -p "$BIN_DIR"
cp -f "$RUST_BIN" "$BIN_PATH"
chmod 755 "$BIN_PATH"

# Add to PATH if needed
if ! echo "$PATH" | tr ':' '\n' | grep -qF "$BIN_DIR"; then
    for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
        if [ -f "$rc" ]; then
            if ! grep -qF "$BIN_DIR" "$rc"; then
                echo "export PATH=\"$BIN_DIR:\$PATH\"" >> "$rc"
                echo "    Added $BIN_DIR to $rc"
            fi
        fi
    done
fi

BIN_SIZE=$(du -h "$BIN_PATH" | cut -f1)
echo ""
echo "==> Done! $BIN_PATH ($BIN_SIZE)"
echo "    Version: $($BIN_PATH --version 2>/dev/null || echo '...')"
echo ""
echo "    Usage:"
echo "      codeseek init                # build index"
echo "      codeseek search <query>      # semantic search"
echo "      codeseek callgraph <symbol>  # query call graph with depth"
echo "      codeseek status              # index status"
echo "      codeseek install             # register with Claude Code"
echo ""
echo "    Tip: run 'source ~/.zshrc' if codeseek not found"
