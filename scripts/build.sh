#!/usr/bin/env bash
# Build xdebbied with bootstrap functionality

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$SCRIPT_DIR/src/xdebbie"
TARGET_DIR="$SRC_DIR/target/release"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# Check for Rust toolchain
if ! command -v cargo &> /dev/null; then
    error "Cargo not found. Please install Rust toolchain:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

info "Rust version: $(rustc --version)"
info "Cargo version: $(cargo --version)"

# Navigate to source directory
cd "$SRC_DIR"

# Check if we're in the right place
if [[ ! -f "Cargo.toml" ]]; then
    error "Not in xdebbie source directory!"
    exit 1
fi

info "Building xdebbied in release mode..."
cargo build --release --package xdebbied

if [[ ! -f "$TARGET_DIR/xdebbied" ]]; then
    error "Build failed! Binary not found at $TARGET_DIR/xdebbied"
    exit 1
fi

# Get binary info
BINARY_SIZE=$(du -h "$TARGET_DIR/xdebbied" | cut -f1)
info "Build successful!"
info "Binary location: $TARGET_DIR/xdebbied"
info "Binary size: $BINARY_SIZE"

# Optionally strip the binary
if command -v strip &> /dev/null; then
    info "Stripping debug symbols..."
    strip "$TARGET_DIR/xdebbied"
    STRIPPED_SIZE=$(du -h "$TARGET_DIR/xdebbied" | cut -f1)
    info "Stripped size: $STRIPPED_SIZE"
fi

# Test binary
info "Testing binary..."
"$TARGET_DIR/xdebbied" --version || {
    error "Binary test failed!"
    exit 1
}

echo ""
info "Build complete! Next steps:"
echo ""
echo "  # Install locally:"
echo "  sudo cp $TARGET_DIR/xdebbied /usr/local/bin/"
echo ""
echo "  # Bootstrap the system:"
echo "  sudo xdebbied bootstrap"
echo ""
echo "  # Start the server:"
echo "  sudo xdebbied serve"
echo ""
echo "  # Configure client:"
echo "  source ~/.xdebbie_env"
echo "  xdctl host ls"
echo ""

# Offer to install
read -p "Install to /usr/local/bin now? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    info "Installing to /usr/local/bin/xdebbied..."
    sudo cp "$TARGET_DIR/xdebbied" /usr/local/bin/
    sudo chmod 755 /usr/local/bin/xdebbied
    info "Installed! Run: sudo xdebbied bootstrap"
fi
