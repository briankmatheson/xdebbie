#!/bin/bash
# Test qfodufk8sd bootstrap
# Run this manually in your terminal: ./test-bootstrap.sh

set -e

BINARY="$HOME/src/qfodufk8s/target/release/qfodufk8sd"

echo "=== Testing qfodufk8sd Bootstrap ==="
echo ""
echo "Binary: $BINARY"
echo "Version: $($BINARY --version)"
echo ""

# Check if we have the necessary commands already
echo "Checking for existing commands..."
which virsh 2>/dev/null && echo "✓ virsh found" || echo "✗ virsh not found"
which qemu-system-x86_64 2>/dev/null && echo "✓ qemu-system-x86_64 found" || echo "✗ qemu-system-x86_64 not found"
which cloud-localds 2>/dev/null && echo "✓ cloud-localds found" || echo "✗ cloud-localds not found"
echo ""

# Check libvirtd status
echo "Checking libvirtd..."
systemctl is-active libvirtd >/dev/null 2>&1 && echo "✓ libvirtd is running" || echo "✗ libvirtd is not running"
echo ""

# Check existing libvirt resources
echo "Existing libvirt networks:"
sudo virsh net-list --all 2>/dev/null || echo "  (none or not accessible)"
echo ""

echo "Existing storage pools:"
sudo virsh pool-list --all 2>/dev/null || echo "  (none or not accessible)"
echo ""

echo "================================================================"
echo "Ready to run bootstrap!"
echo ""
echo "Run this command:"
echo "  sudo $BINARY bootstrap"
echo ""
echo "This will:"
echo "  1. Install missing packages (if any)"
echo "  2. Start libvirtd (if not running)"
echo "  3. Create qfodufk8s network and pool"
echo "  4. Generate /etc/qfodufk8s/qfodufk8sd.toml"
echo "  5. Generate /etc/qfodufk8s/token"
echo "  6. Create ~/.qfodufk8s_env"
echo ""
echo "Press Enter to run bootstrap now, or Ctrl+C to abort..."
read

echo ""
echo "Running: sudo $BINARY bootstrap"
echo ""
sudo "$BINARY" bootstrap

echo ""
echo "================================================================"
echo "Bootstrap complete! Checking results..."
echo ""

# Verify what was created
echo "Configuration file:"
sudo cat /etc/qfodufk8s/qfodufk8sd.toml | head -20
echo ""

echo "Token file:"
ls -la /etc/qfodufk8s/token
echo ""

echo "Environment file:"
cat ~/.qfodufk8s_env
echo ""

echo "Networks:"
sudo virsh net-list --all
echo ""

echo "Storage pools:"
sudo virsh pool-list --all
echo ""

echo "================================================================"
echo "✓ Bootstrap successful!"
echo ""
echo "Next steps:"
echo ""
echo "1. Start the server in one terminal:"
echo "   sudo $BINARY serve"
echo ""
echo "2. Test client in another terminal:"
echo "   source ~/.qfodufk8s_env"
echo "   qfodufk8sctl host ls"
echo ""
