#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Bake a Kubernetes node image from the Debian 13 generic cloud image.
# Runs on any Debian box with libguestfs-tools (your workstation is fine).
#
#   ./build-image.sh v1.36        # Kubernetes minor (pkgs.k8s.io channel)
#
# Produces debian-13-k8s-<minor>.qcow2 containing containerd (systemd
# cgroups), kubeadm/kubelet/kubectl, qemu-guest-agent and the usual sysctls.
set -euo pipefail
MINOR=${1:?usage: $0 v1.<minor>}
BASE=debian-13-generic-amd64.qcow2
OUT=debian-13-k8s-${MINOR}.qcow2
URL=https://cloud.debian.org/images/cloud/trixie/latest/$BASE

command -v virt-customize >/dev/null || { echo "apt install libguestfs-tools" >&2; exit 1; }
[[ -f $BASE ]] || curl -fLO "$URL"
cp "$BASE" "$OUT"

virt-customize -a "$OUT" \
  --install qemu-guest-agent,containerd,apt-transport-https,ca-certificates,curl,gpg,conntrack,socat,ebtables,ethtool \
  --run-command "mkdir -p /etc/apt/keyrings && curl -fsSL https://pkgs.k8s.io/core:/stable:/${MINOR}/deb/Release.key | gpg --dearmor -o /etc/apt/keyrings/kubernetes.gpg" \
  --write "/etc/apt/sources.list.d/kubernetes.list:deb [signed-by=/etc/apt/keyrings/kubernetes.gpg] https://pkgs.k8s.io/core:/stable:/${MINOR}/deb/ /" \
  --run-command "apt-get update && apt-get install -y kubelet kubeadm kubectl && apt-mark hold kubelet kubeadm kubectl" \
  --write "/etc/modules-load.d/k8s.conf:overlay
br_netfilter" \
  --write "/etc/sysctl.d/99-k8s.conf:net.bridge.bridge-nf-call-iptables = 1
net.bridge.bridge-nf-call-ip6tables = 1
net.ipv4.ip_forward = 1" \
  --run-command "mkdir -p /etc/containerd && containerd config default > /etc/containerd/config.toml && sed -i 's/SystemdCgroup = false/SystemdCgroup = true/' /etc/containerd/config.toml" \
  --run-command "systemctl enable containerd kubelet qemu-guest-agent" \
  --run-command "cloud-init clean --logs --seed || true" \
  --truncate /etc/machine-id

virt-sparsify --in-place "$OUT"
echo ">> built $OUT ($(du -h "$OUT" | cut -f1))"
