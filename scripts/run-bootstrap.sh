#!/bin/bash
# Script to run xdebbied bootstrap
# This requires root privileges to install packages and configure libvirt

set -e

BINARY="$HOME/src/xdebbie/target/release/xdebbied"

echo "=== xdebbied Bootstrap ==="
echo ""
echo "This will:"
echo "  1. Install libvirt, QEMU, cloud-localds (if missing)"
echo "  2. Start and enable libvirtd"
echo "  3. Create xdebbie network (192.168.100.0/24)"
echo "  4. Create xdebbie storage pool (/var/lib/libvirt/xdebbie)"
echo "  5. Generate config (/etc/xdebbie/xdebbied.toml)"
echo "  6. Generate token (/etc/xdebbie/token)"
echo "  7. Create env file (~/.xdebbie_env)"
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
echo "  source ~/.xdebbie_env"
echo "  xdctl host ls"
