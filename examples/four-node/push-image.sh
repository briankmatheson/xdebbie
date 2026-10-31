#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Upload a base image into the `default` pool on every host, via libvirt.
#   ./push-image.sh debian-13-k8s-v1.36.qcow2 srv1 srv2 srv3 srv4
set -euo pipefail
IMG=${1:?image}; shift
NAME=$(basename "$IMG" .qcow2)
BYTES=$(stat -c %s "$IMG")
for h in "$@"; do
  URI="qemu+ssh://qfodufk8s@$h/system"
  if virsh -q -c "$URI" vol-info --pool default "$NAME" >/dev/null 2>&1; then
    echo "$h: $NAME already present"; continue
  fi
  virsh -q -c "$URI" vol-create-as default "$NAME" "$BYTES" --format raw
  virsh -q -c "$URI" vol-upload --pool default "$NAME" "$IMG"
  virsh -q -c "$URI" pool-refresh default
  echo "$h: uploaded $NAME"
done
