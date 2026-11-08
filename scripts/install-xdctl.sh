#!/bin/bash
# Install xdctl CLI tool

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

GREEN='\033[0;32m'
NC='\033[0m'

info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

cd "$REPO_DIR"

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "Error: cargo not found. Install Rust first:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

# Build xdctl
info "Building xdctl..."
cargo build --release --package xdctl

# Install
info "Installing xdctl to /usr/local/bin/"
sudo cp target/release/xdctl /usr/local/bin/
sudo chmod 755 /usr/local/bin/xdctl

# Verify
info "Verifying installation..."
xdctl --version

echo ""
info "xdctl installed successfully!"
echo ""
echo "Usage:"
echo "  # Set environment variables"
echo "  source ~/.xdebbie_env"
echo ""
echo "  # Or for old qfodufk8s install:"
echo "  export XDEBBIE_URL=\$QFODUFK8S_URL"
echo "  export XDEBBIE_TOKEN=\$QFODUFK8S_TOKEN"
echo ""
echo "  # Then use xdctl"
echo "  xdctl host ls"
echo "  xdctl vm ls"
echo "  xdctl vm create --help"
echo ""
