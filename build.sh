#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="$SCRIPT_DIR/rust-core"
BIN_DIR="$HOME/.codeseek/bin"
BIN_PATH="$BIN_DIR/codeseek"

echo "==> [1/2] Building TypeScript wrapper..."
cd "$SCRIPT_DIR"
npx tsc
echo "    dist/ ready"

echo ""
echo "==> [2/2] Building Rust binary..."
cd "$RUST_DIR"

# Use debug build for faster iteration; switch to --release for production
if [ "${1:-}" = "--release" ]; then
    cargo build --release
    RUST_BIN="$RUST_DIR/target/release/codeseek"
else
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
    SHELL_RC="$HOME/.zshrc"
    [ -f "$HOME/.bashrc" ] && SHELL_RC="$HOME/.bashrc"
    echo "export PATH=\"$BIN_DIR:\$PATH\"" >> "$SHELL_RC"
    echo "    Added $BIN_DIR to $SHELL_RC"
fi

echo ""
echo "==> Done!"
echo "    Binary:  $BIN_PATH"
echo "    Version: $($BIN_PATH --version 2>/dev/null || echo '...')"
echo ""
echo "    Usage:"
echo "      codeseek init              # build index"
echo "      codeseek search <query>    # semantic search"
echo "      codeseek status            # index status"
echo "      codeseek callers <symbol>  # find callers"
echo ""
echo "    Tip: run 'source ~/.zshrc' first if codeseek not found"
