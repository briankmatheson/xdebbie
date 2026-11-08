#!/usr/bin/env bash
# Build qfodufk8sd with bootstrap functionality

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$SCRIPT_DIR/src/qfodufk8s"
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
    error "Not in qfodufk8s source directory!"
    exit 1
fi

info "Building qfodufk8sd in release mode..."
cargo build --release --package qfodufk8sd

if [[ ! -f "$TARGET_DIR/qfodufk8sd" ]]; then
    error "Build failed! Binary not found at $TARGET_DIR/qfodufk8sd"
    exit 1
fi

# Get binary info
BINARY_SIZE=$(du -h "$TARGET_DIR/qfodufk8sd" | cut -f1)
info "Build successful!"
info "Binary location: $TARGET_DIR/qfodufk8sd"
info "Binary size: $BINARY_SIZE"

# Optionally strip the binary
if command -v strip &> /dev/null; then
    info "Stripping debug symbols..."
    strip "$TARGET_DIR/qfodufk8sd"
    STRIPPED_SIZE=$(du -h "$TARGET_DIR/qfodufk8sd" | cut -f1)
    info "Stripped size: $STRIPPED_SIZE"
fi

# Test binary
info "Testing binary..."
"$TARGET_DIR/qfodufk8sd" --version || {
    error "Binary test failed!"
    exit 1
}

echo ""
info "Build complete! Next steps:"
echo ""
echo "  # Install locally:"
echo "  sudo cp $TARGET_DIR/qfodufk8sd /usr/local/bin/"
echo ""
echo "  # Bootstrap the system:"
echo "  sudo qfodufk8sd bootstrap"
echo ""
echo "  # Start the server:"
echo "  sudo qfodufk8sd serve"
echo ""
echo "  # Configure client:"
echo "  source ~/.qfodufk8s_env"
echo "  qfodufk8sctl host ls"
echo ""

# Offer to install
read -p "Install to /usr/local/bin now? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    info "Installing to /usr/local/bin/qfodufk8sd..."
    sudo cp "$TARGET_DIR/qfodufk8sd" /usr/local/bin/
    sudo chmod 755 /usr/local/bin/qfodufk8sd
    info "Installed! Run: sudo qfodufk8sd bootstrap"
fi
