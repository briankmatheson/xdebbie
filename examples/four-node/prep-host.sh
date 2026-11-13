#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prepare a Debian 13 (trixie) server as a XDebbie8s hypervisor host.
#
#   sudo ./prep-host.sh --nic eno1 --pubkey "ssh-ed25519 AAAA... xdebbied@srv1"
#
# Idempotent. Installs KVM/libvirt, creates the `xdebbie` user that xdebbied
# connects as, defines the `default` dir pool, and (optionally) converts
# --nic into a Linux bridge `br0` with DHCP.
set -euo pipefail

NIC="" PUBKEY="" BRIDGE=br0 POOL_DIR=/var/lib/libvirt/images
while [[ $# -gt 0 ]]; do
  case $1 in
    --nic) NIC=$2; shift 2;;
    --pubkey) PUBKEY=$2; shift 2;;
    --bridge) BRIDGE=$2; shift 2;;
    --pool-dir) POOL_DIR=$2; shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }
grep -qE '(vmx|svm)' /proc/cpuinfo || { echo "no hardware virtualization; enable VT-x/AMD-V in firmware" >&2; exit 1; }

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -yq --no-install-recommends \
  qemu-system-x86 qemu-utils libvirt-daemon-system libvirt-clients \
  bridge-utils ifupdown chrony

# The account xdebbied uses. libvirt's ssh transport runs virt-ssh-helper as it.
id xdebbie >/dev/null 2>&1 || useradd --system --create-home --home-dir /var/lib/xdebbie-host --shell /bin/bash xdebbie
usermod -aG libvirt,kvm xdebbie
install -d -m 700 -o xdebbie -g xdebbie /var/lib/xdebbie-host/.ssh
if [[ -n $PUBKEY ]]; then
  grep -qxF "$PUBKEY" /var/lib/xdebbie-host/.ssh/authorized_keys 2>/dev/null || echo "$PUBKEY" >> /var/lib/xdebbie-host/.ssh/authorized_keys
  chown xdebbie:xdebbie /var/lib/xdebbie-host/.ssh/authorized_keys; chmod 600 /var/lib/xdebbie-host/.ssh/authorized_keys
fi

# Storage pool for base images and VM overlays.
if ! virsh -q pool-info default >/dev/null 2>&1; then
  virsh pool-define-as default dir --target "$POOL_DIR"
  virsh pool-build default
fi
virsh pool-start default 2>/dev/null || true
virsh pool-autostart default

# Bridge. Only touched when --nic is given; keep a console handy.
if [[ -n $NIC ]] && ! ip link show "$BRIDGE" >/dev/null 2>&1; then
  cp /etc/network/interfaces /etc/network/interfaces.pre-xdebbie
  cat > /etc/network/interfaces <<EOT
source /etc/network/interfaces.d/*
auto lo
iface lo inet loopback

iface $NIC inet manual

auto $BRIDGE
iface $BRIDGE inet dhcp
    bridge_ports $NIC
    bridge_stp off
    bridge_fd 0
EOT
  echo ">> bridge $BRIDGE configured on $NIC; restarting networking"
  systemctl restart networking
fi

# Live migration data channel (QEMU picks from this range).
if command -v nft >/dev/null && nft list ruleset 2>/dev/null | grep -q 'hook input'; then
  echo ">> nftables active: allow tcp 49152-49215 between hosts for live migration"
fi

systemctl enable --now libvirtd

# Strip desktop services that cause idle sleep, spurious wakeups, and
# wasted RAM on a headless hypervisor.  Each is only disabled/masked if
# it is actually present so the script stays idempotent on a minimal install.
DESKTOP_UNITS=(
  gdm.service           # GNOME display manager → triggers idle suspend
  sddm.service          # KDE display manager
  lightdm.service       # LightDM
  power-profiles-daemon.service  # desktop power profiles (conflicts with tuned)
  upower.service        # desktop battery/power events
  bluetooth.service     # no BT needed on a hypervisor
  cups.service          # printing
  cups-browsed.service  # network printer discovery
)
for unit in "${DESKTOP_UNITS[@]}"; do
  if systemctl list-unit-files "$unit" 2>/dev/null | grep -q "$unit"; then
    systemctl disable --now "$unit" 2>/dev/null || true
    systemctl mask "$unit"
    echo ">> masked $unit"
  fi
done

# Switch to multi-user (text) target so a reboot doesn't re-launch a DM.
systemctl set-default multi-user.target

# Prevent systemd-logind from sleeping the box when the last session ends
# or the lid is closed (common on repurposed laptops/mini-ITX desktops).
mkdir -p /etc/systemd/logind.conf.d
cat > /etc/systemd/logind.conf.d/xdebbie-nosleep.conf <<'EOF'
[Login]
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
IdleAction=ignore
EOF
systemctl restart systemd-logind

echo ">> $(hostname) ready: $(nproc) cpus, $(free -g | awk '/Mem:/{print $2}') GiB RAM"
