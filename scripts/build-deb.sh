#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the xdebbied .deb the way CI does (.drone.yaml): both binaries from
# this tree, then package them. The version is the workspace's, in
# Cargo.toml; nothing here changes it.
set -eu
cd "$(dirname "$0")/.."
command -v cargo-deb >/dev/null || { echo "cargo-deb is missing: cargo install cargo-deb --locked" >&2; exit 1; }
cargo build --release -p xdebbied -p xdctl
cargo deb -p xdebbied --no-build
ls -l "${CARGO_TARGET_DIR:-target}"/debian/xdebbied_*.deb
