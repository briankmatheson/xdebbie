#!/bin/bash
# Script to run qfodufk8sd bootstrap
# This requires root privileges to install packages and configure libvirt

set -e

BINARY="$HOME/src/qfodufk8s/target/release/qfodufk8sd"

echo "=== qfodufk8sd Bootstrap ==="
echo ""
echo "This will:"
echo "  1. Install libvirt, QEMU, cloud-localds (if missing)"
echo "  2. Start and enable libvirtd"
echo "  3. Create qfodufk8s network (192.168.100.0/24)"
echo "  4. Create qfodufk8s storage pool (/var/lib/libvirt/qfodufk8s)"
echo "  5. Generate config (/etc/qfodufk8s/qfodufk8sd.toml)"
echo "  6. Generate token (/etc/qfodufk8s/token)"
echo "  7. Create env file (~/.qfodufk8s_env)"
echo ""
echo "Press Enter to continue, Ctrl+C to abort..."
read

echo "Running bootstrap..."
sudo "$BINARY" bootstrap

echo ""
echo "=== Bootstrap Complete! ==="
echo ""
echo "To start the server:"
echo "  sudo $BINARY serve"
echo ""
echo "To configure client (in a new terminal):"
echo "  source ~/.qfodufk8s_env"
echo "  qfodufk8sctl host ls"
