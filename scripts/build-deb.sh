#!/bin/bash
# Build xdebbied Debian package

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

cd "$REPO_DIR"

# Get version from git tag
GIT_VERSION=$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
if [ -n "$GIT_VERSION" ]; then
    info "Using version from git tag: $GIT_VERSION"
    # Update workspace Cargo.toml with git version
    sed -i "s/^version = \".*\"/version = \"$GIT_VERSION\"/" Cargo.toml
else
    warn "No git tags found, using version from Cargo.toml"
fi

info "Building xdebbied Debian package..."
echo ""

# Check for cargo-deb
if ! command -v cargo-deb &> /dev/null; then
    warn "cargo-deb not found. Installing..."
    cargo install cargo-deb
fi

# Check for source .cargo/env
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

# Build release binaries first
info "Building release binaries..."
cargo build --release --package xdebbied
cargo build --release --package xdctl

# Strip binary to reduce size
if command -v strip &> /dev/null; then
    info "Stripping debug symbols..."
    strip target/release/xdebbied
fi

# Build .deb package
info "Building Debian package..."
cargo deb --package xdebbied --no-build

# Find the generated package
DEB_FILE=$(ls -t target/debian/xdebbied_*.deb | head -1)

if [ ! -f "$DEB_FILE" ]; then
    error "Failed to find generated .deb file!"
    exit 1
fi

DEB_SIZE=$(du -h "$DEB_FILE" | cut -f1)

info "Package built successfully!"
echo ""
echo "  File: $DEB_FILE"
echo "  Size: $DEB_SIZE"
echo ""

# Extract package info
info "Package information:"
dpkg-deb -I "$DEB_FILE" | grep -E "Package:|Version:|Architecture:|Depends:"

echo ""
info "Package contents:"
dpkg-deb -c "$DEB_FILE" | grep -E "usr/bin|systemd|doc"

echo ""
echo "========================================="
echo "Installation Instructions:"
echo "========================================="
echo ""
echo "1. Install on this system:"
echo "   sudo dpkg -i $DEB_FILE"
echo "   sudo apt-get install -f  # Fix dependencies if needed"
echo ""
echo "2. Check service status:"
echo "   systemctl status xdebbied"
echo ""
echo "3. View logs:"
echo "   journalctl -u xdebbied -f"
echo ""
echo "4. Configure client (as your user):"
echo "   source ~/.xdebbie_env"
echo "   xdctl host ls"
echo ""
echo "5. Copy to other systems:"
echo "   scp $DEB_FILE user@host:/tmp/"
echo "   ssh user@host sudo dpkg -i /tmp/$(basename $DEB_FILE)"
echo ""
echo "========================================="
echo "Upgrade Instructions:"
echo "========================================="
echo ""
echo "   sudo dpkg -i $DEB_FILE"
echo "   # Service automatically restarts"
echo ""
echo "========================================="
echo "Removal Instructions:"
echo "========================================="
echo ""
echo "Remove (keep config):"
echo "   sudo apt-get remove xdebbied"
echo ""
echo "Purge (remove config):"
echo "   sudo apt-get purge xdebbied"
echo ""
