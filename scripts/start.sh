#!/bin/bash
# Start qfodufk8sd server

BINARY="$HOME/src/qfodufk8s/target/release/qfodufk8sd"

echo "=== Starting qfodufk8sd ==="
echo ""
echo "Server will listen on: 0.0.0.0:7443"
echo "Config: /etc/qfodufk8s/qfodufk8sd.toml"
echo "Token: /etc/qfodufk8s/token"
echo ""
echo "Press Ctrl+C to stop the server"
echo ""
echo "In another terminal, test with:"
echo "  source ~/.qfodufk8s_env"
echo "  qfodufk8sctl host ls"
echo ""
echo "Starting..."
echo ""

sudo "$BINARY" serve
