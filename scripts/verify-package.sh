#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Check a built xdebbied .deb: control fields, maintainer scripts, files.
#   scripts/verify-package.sh [DEB]   (default: the newest under target/debian)
set -eu
cd "$(dirname "$0")/.."
deb=${1:-$(ls -t "${CARGO_TARGET_DIR:-target}"/debian/xdebbied_*.deb 2>/dev/null | head -1)}
[ -n "$deb" ] && [ -f "$deb" ] || { echo "no package: run scripts/build-deb.sh first" >&2; exit 1; }
want=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
dpkg-deb -e "$deb" "$work/ctl"; dpkg-deb -x "$deb" "$work/root"
fail=0
check() { if eval "$2"; then echo "ok    $1"; else echo "FAIL  $1"; fail=1; fi; }
ver=$(dpkg-deb -f "$deb" Version)
check "version $ver matches Cargo.toml $want" '[ "${ver%-*}" = "$want" ]'
check "maintainer has an address" 'dpkg-deb -f "$deb" Maintainer | grep -q "<.*@.*>"'
for s in postinst prerm postrm; do
  check "$s is valid sh" 'sh -n "$work/ctl/$s"'
  check "$s has cargo-deb's systemd section exactly once" '[ "$(grep -c "deb-systemd-invoke" "$work/ctl/$s")" -le 1 ] && grep -q "Automatically added by cargo-deb" "$work/ctl/$s"'
done
for f in usr/bin/xdebbied usr/bin/xdctl usr/lib/systemd/system/xdebbied.service \
         etc/modules-load.d/xdebbie-mesh.conf etc/apparmor.d/local/wg \
         usr/share/doc/xdebbied/copyright usr/share/doc/xdebbied/README.md \
         usr/share/doc/xdebbied/BOOTSTRAP.md usr/share/doc/xdebbied/mesh.md \
         usr/share/doc/xdebbied/network.md usr/share/doc/xdebbied/troubleshooting.md \
         usr/share/doc/xdebbied/configuration.md; do
  check "ships /$f" '[ -e "$work/root/$f" ]'
done
check "no docs outside /usr/share/doc/xdebbied" '[ ! -e "$work/root/usr/share/doc/xdebbie" ]'
check "conffiles listed" 'grep -qx /etc/apparmor.d/local/wg "$work/ctl/conffiles" && grep -qx /etc/modules-load.d/xdebbie-mesh.conf "$work/ctl/conffiles"'
check "xdctl runs" '"$work/root/usr/bin/xdctl" --version >/dev/null'
exit $fail
