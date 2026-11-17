#!/bin/bash
# Start xdebbied server

BINARY="$HOME/src/xdebbie/target/release/xdebbied"

echo "=== Starting xdebbied ==="
echo ""
echo "Server will listen on: 0.0.0.0:23443"
echo "Config: /etc/xdebbie/xdebbied.toml"
echo "Token: /etc/xdebbie/token"
echo ""
echo "Press Ctrl+C to stop the server"
echo ""
echo "In another terminal, test with:"
echo "  source ~/.xdebbie_env"
echo "  xdctl host ls"
echo ""
echo "Starting..."
echo ""

sudo "$BINARY" serve
