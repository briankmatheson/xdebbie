#!/bin/bash
# Automated rename from xdebbie to XDebbie

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

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

echo "======================================================"
echo "  🎮 Renaming xdebbie → XDebbie"
echo "======================================================"
echo ""
echo "This will rename:"
echo "  • xdebbie → xdebbie"
echo "  • xdebbied → xdebbied"
echo "  • xdctl → xdctl"
echo "  • CAPQ → CAPX"
echo ""
echo "Current directory: $REPO_DIR"
echo ""

read -p "Continue? (yes/no) " -r
echo
if [[ ! $REPLY =~ ^[Yy]es$ ]]; then
    echo "Aborted."
    exit 1
fi

info "Step 1: Creating checkpoint..."
git add -A || true
git commit -m "checkpoint: before rename to XDebbie" || true
git tag pre-rename-to-xdebbie || true

info "Step 2: Renaming crate directories..."
[ -d crates/xdebbied ] && mv crates/xdebbied crates/xdebbied
[ -d crates/xdctl ] && mv crates/xdctl crates/xdctl
[ -d crates/xdebbie-core ] && mv crates/xdebbie-core crates/xdebbie-core
[ -d crates/capq ] && mv crates/capq crates/capx

info "Step 3: Updating Cargo.toml files..."
find . -name Cargo.toml -type f | while read f; do
    sed -i 's/xdebbie-core/xdebbie-core/g' "$f"
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/capq/capx/g' "$f"
    sed -i 's/CAPQ/CAPX/g' "$f"
done

info "Step 4: Updating Rust source files..."
find crates/ -name "*.rs" -type f | while read f; do
    sed -i 's/xdebbie_core/xdebbie_core/g' "$f"
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/XDEBBIE/XDEBBIE/g' "$f"
    sed -i 's/Qfodufk/XDebbie/g' "$f"
    sed -i 's/capq/capx/g' "$f"
    sed -i 's/CAPQ/CAPX/g' "$f"
done

info "Step 5: Updating documentation..."
find docs/ -name "*.md" -type f 2>/dev/null | while read f; do
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/XDEBBIE/XDEBBIE/g' "$f"
    sed -i 's/Qfodufk/XDebbie/g' "$f"
    sed -i 's/CAPQ/CAPX/g' "$f"
done || true

# Update main README
[ -f README.md ] && sed -i 's/xdebbied/xdebbied/g' README.md
[ -f README.md ] && sed -i 's/xdctl/xdctl/g' README.md
[ -f README.md ] && sed -i 's/xdebbie/xdebbie/g' README.md
[ -f README.md ] && sed -i 's/XDEBBIE/XDEBBIE/g' README.md
[ -f README.md ] && sed -i 's/Qfodufk/XDebbie/g' README.md

# Update BOOTSTRAP.md
[ -f BOOTSTRAP.md ] && sed -i 's/xdebbied/xdebbied/g' BOOTSTRAP.md
[ -f BOOTSTRAP.md ] && sed -i 's/xdctl/xdctl/g' BOOTSTRAP.md
[ -f BOOTSTRAP.md ] && sed -i 's/xdebbie/xdebbie/g' BOOTSTRAP.md
[ -f BOOTSTRAP.md ] && sed -i 's/XDEBBIE/XDEBBIE/g' BOOTSTRAP.md

info "Step 6: Updating scripts..."
find scripts/ -name "*.sh" -type f 2>/dev/null | while read f; do
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/XDEBBIE/XDEBBIE/g' "$f"
done || true

info "Step 7: Renaming systemd service file..."
if [ -f crates/xdebbied/debian/xdebbied.service ]; then
    mv crates/xdebbied/debian/xdebbied.service crates/xdebbied/debian/xdebbied.service
fi

info "Step 8: Updating systemd service content..."
if [ -f crates/xdebbied/debian/xdebbied.service ]; then
    sed -i 's/xdebbied/xdebbied/g' crates/xdebbied/debian/xdebbied.service
    sed -i 's/xdebbie/xdebbie/g' crates/xdebbied/debian/xdebbied.service
fi

info "Step 9: Updating debian maintainer scripts..."
find crates/xdebbied/debian/ -type f 2>/dev/null | while read f; do
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/XDEBBIE/XDEBBIE/g' "$f"
done || true

info "Step 10: Updating examples and tests..."
find examples/ -type f 2>/dev/null | while read f; do
    sed -i 's/xdebbied/xdebbied/g' "$f"
    sed -i 's/xdctl/xdctl/g' "$f"
    sed -i 's/xdebbie/xdebbie/g' "$f"
    sed -i 's/Qfodufk/XDebbie/g' "$f"
done || true

info "Step 11: Cleaning build artifacts..."
cargo clean || true
rm -rf target/debian/*.deb || true

echo ""
echo "======================================================"
echo "  ✅ Rename Complete!"
echo "======================================================"
echo ""
info "Summary of changes:"
echo "  • All crate names updated"
echo "  • All source files updated"
echo "  • Documentation updated"
echo "  • Scripts updated"
echo "  • Debian package files updated"
echo ""
info "Next steps:"
echo ""
echo "  1. Review changes:"
echo "     git status"
echo "     git diff"
echo ""
echo "  2. Test build:"
echo "     cargo build --release"
echo ""
echo "  3. Build Debian package:"
echo "     ./scripts/build-deb.sh"
echo ""
echo "  4. Commit:"
echo "     git add -A"
echo "     git commit -m 'Rename xdebbie to XDebbie'"
echo "     git tag v0.2.0"
echo ""
echo "  5. Rename repository directory:"
echo "     cd ~/src/"
echo "     mv xdebbie xdebbie"
echo ""
echo "  6. Update git remote (if needed):"
echo "     cd ~/src/xdebbie"
echo "     git remote set-url origin https://github.com/USER/xdebbie.git"
echo ""
echo "======================================================"
echo "  🎮 Welcome to XDebbie!"
echo "======================================================"
