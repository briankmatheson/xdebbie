#!/bin/bash
set -euo pipefail

PACKAGE="target/debian/xdebbied_0.3.0-1_amd64.deb"

echo "=== XDebbie v0.3.0 Package Verification ==="
echo

if [ ! -f "$PACKAGE" ]; then
    echo "❌ Package not found: $PACKAGE"
    echo "Run ./scripts/build-deb.sh first"
    exit 1
fi

echo "✅ Package exists: $PACKAGE"
echo

echo "=== Package Info ==="
dpkg-deb -I "$PACKAGE" | grep -E "(Package|Version|Architecture|Depends|Description)" | head -20
echo

echo "=== Package Contents ==="
echo "Binaries:"
dpkg-deb -c "$PACKAGE" | grep "/usr/bin/"
echo
echo "Documentation:"
dpkg-deb -c "$PACKAGE" | grep "/usr/share/doc/"
echo
echo "Systemd:"
dpkg-deb -c "$PACKAGE" | grep "systemd"
echo

echo "=== File Sizes ==="
dpkg-deb -c "$PACKAGE" | grep "/usr/bin/" | awk '{print $3, $6}' | while read size file; do
    mb=$(echo "scale=1; $size / 1024 / 1024" | bc)
    echo "$file: ${mb} MB"
done
echo

echo "=== Discovery Features Check ==="
if dpkg-deb -I "$PACKAGE" | grep -q "Self-discovery via multicast"; then
    echo "✅ Package description mentions discovery"
else
    echo "❌ Package description missing discovery info"
fi

if dpkg-deb -I "$PACKAGE" | grep -q "Leader election"; then
    echo "✅ Package description mentions leader election"
else
    echo "❌ Package description missing election info"
fi
echo

echo "=== Version Check ==="
VERSION=$(dpkg-deb -f "$PACKAGE" Version | cut -d'-' -f1)
if [ "$VERSION" = "0.3.0" ]; then
    echo "✅ Version is 0.3.0"
else
    echo "❌ Version is $VERSION (expected 0.3.0)"
fi
echo

echo "=== Binary Check ==="
# Extract and check binaries
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

dpkg-deb -x "$PACKAGE" "$TMPDIR"

if [ -f "$TMPDIR/usr/bin/xdebbied" ]; then
    echo "✅ xdebbied binary exists"
    echo "   Size: $(du -h $TMPDIR/usr/bin/xdebbied | cut -f1)"
else
    echo "❌ xdebbied binary missing"
fi

if [ -f "$TMPDIR/usr/bin/xdctl" ]; then
    echo "✅ xdctl binary exists"
    echo "   Size: $(du -h $TMPDIR/usr/bin/xdctl | cut -f1)"
else
    echo "❌ xdctl binary missing"
fi
echo

echo "=== Documentation Check ==="
if [ -f "$TMPDIR/usr/share/doc/xdebbie/README.md" ]; then
    echo "✅ README.md included"
else
    echo "❌ README.md missing"
fi

if [ -f "$TMPDIR/usr/share/doc/xdebbie/BOOTSTRAP.md" ]; then
    echo "✅ BOOTSTRAP.md included"
else
    echo "❌ BOOTSTRAP.md missing"
fi
echo

echo "=== Systemd Service Check ==="
if [ -f "$TMPDIR/usr/lib/systemd/system/xdebbied.service" ]; then
    echo "✅ Systemd service file exists"
    echo "   Checking dependencies..."
    if grep -q "After=network-online.target libvirtd.service" "$TMPDIR/usr/lib/systemd/system/xdebbied.service"; then
        echo "   ✅ Network and libvirtd dependencies configured"
    else
        echo "   ❌ Missing network/libvirtd dependencies"
    fi
    if grep -q "ExecStartPre=/usr/bin/xdebbied bootstrap" "$TMPDIR/usr/lib/systemd/system/xdebbied.service"; then
        echo "   ✅ Bootstrap pre-start configured"
    else
        echo "   ❌ Missing bootstrap pre-start"
    fi
else
    echo "❌ Systemd service file missing"
fi
echo

echo "=== Summary ==="
echo "Package: $PACKAGE"
echo "Size: $(du -h $PACKAGE | cut -f1)"
echo "Version: $VERSION"
echo
echo "✅ Verification complete!"
