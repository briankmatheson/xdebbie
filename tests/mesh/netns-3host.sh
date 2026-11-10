#!/usr/bin/env bash
# Three "hypervisors" on one machine for the Geneve mesh tests (spec 11.2).
#
#   netns h0, h1, h2:
#     ul0     veth into a shared "lan" bridge, 10.22.<N>.1/16, MTU 1420
#     br-int  bridge, 10.23.<N>.1/16, MAC 52:54:00:23:0<N>:fe, STP off
#   netns vm0, vm1, vm2:
#     eth0    veth peer enslaved to h<N>'s br-int, 10.23.<N>.11/16,
#             MAC 52:54:00:23:0<N>:01, MTU 1370 (what cloud-init would set)
#
# Usage: netns-3host.sh up|down|daemons|stop-daemons|status
#
# Needs root *or* an unprivileged user namespace: run.sh re-executes
# itself under `unshare -Urnm` when not root, and everything here works
# there too (the geneve module must already be loaded on the host).
set -euo pipefail
export PATH=/usr/sbin:/sbin:$PATH

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
WORK=${MESH_TEST_WORK:-/tmp/xdebbie-mesh}
XDEBBIED=${XDEBBIED:-$ROOT/target/debug/xdebbied}
XDCTL=${XDCTL:-$ROOT/target/debug/xdctl}
HOSTS="0 1 2"

log() { echo "[netns-3host] $*" >&2; }

ensure_netns_mount() {
  # `ip netns` wants /run/netns. In a fresh mount+user namespace /run is
  # read-only for us, so put a tmpfs there.
  if ! mkdir -p /run/netns 2>/dev/null; then
    mount -t tmpfs tmpfs /run
    mkdir -p /run/netns
  fi
}

up() {
  ensure_netns_mount
  mkdir -p "$WORK"
  modprobe geneve 2>/dev/null || true
  [[ -e /sys/module/geneve ]] || { log "geneve module not loaded"; exit 1; }
  ip link set lo up 2>/dev/null || true
  ip link show lan >/dev/null 2>&1 || { ip link add lan type bridge; ip link set lan up; }
  for N in $HOSTS; do
    ip netns list | grep -qw "h$N" && continue
    ip netns add h$N
    ip netns add vm$N
    ip link add ul0 netns h$N type veth peer name lan$N
    ip link set lan$N master lan up
    ip -n h$N link set lo up
    ip -n h$N addr add 10.22.$N.1/16 dev ul0
    ip -n h$N link set ul0 mtu 1420 up
    ip -n h$N link add br-int type bridge stp_state 0
    ip -n h$N link set br-int address 52:54:00:23:0$N:fe
    ip -n h$N addr add 10.23.$N.1/16 dev br-int
    ip -n h$N link set br-int up
    # VM
    ip -n h$N link add vnet0 type veth peer name eth0 netns vm$N
    ip -n h$N link set vnet0 mtu 1370 master br-int up   # libvirt sets the tap MTU from <mtu size=/>
    ip -n vm$N link set lo up
    ip -n vm$N link set eth0 address 52:54:00:23:0$N:01
    ip -n vm$N addr add 10.23.$N.11/16 dev eth0
    ip -n vm$N link set eth0 mtu 1370 up
    mkdir -p "$WORK/h$N"
  done
  log "up: h0 h1 h2 (+ vm0 vm1 vm2) on bridge lan"
}

down() {
  stop_daemons || true
  for N in $HOSTS; do
    ip netns del vm$N 2>/dev/null || true
    ip netns del h$N 2>/dev/null || true
  done
  ip link del lan 2>/dev/null || true
  log "down"
}

daemon_start() {
  local N=$1
  local pidf="$WORK/h$N/xdebbied.pid"
  if [[ -f "$pidf" ]] && kill -0 "$(cat "$pidf")" 2>/dev/null; then
    return 0
  fi
  local cfg="$HERE/h$N.toml"
  : > "$WORK/h$N/xdebbied.log"
  RUST_LOG=${RUST_LOG:-info} ip netns exec h$N "$XDEBBIED" --config "$cfg" serve --mesh-only \
    >>"$WORK/h$N/xdebbied.log" 2>&1 &
  echo $! > "$pidf"
  # Wait for the API.
  for _ in $(seq 1 50); do
    if ip netns exec h$N "$XDCTL" --url http://127.0.0.1:7443 mesh get >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  log "daemon in h$N did not come up; log:"; tail -20 "$WORK/h$N/xdebbied.log" >&2
  return 1
}

daemon_stop() {
  local N=$1
  local pidf="$WORK/h$N/xdebbied.pid"
  if [[ -f "$pidf" ]]; then
    local pid; pid=$(cat "$pidf")
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
      kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -f "$pidf"
  fi
}

daemons() { for N in $HOSTS; do daemon_start $N; done; log "daemons running"; }
stop_daemons() { for N in $HOSTS; do daemon_stop $N; done; }

status() {
  for N in $HOSTS; do
    echo "== h$N =="
    ip -n h$N -br link show type geneve 2>/dev/null || true
    ip netns exec h$N "$XDCTL" --url http://127.0.0.1:7443 mesh ls 2>/dev/null || echo "(daemon not running)"
  done
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  daemons) daemons ;;
  stop-daemons) stop_daemons ;;
  daemon-start) daemon_start "$2" ;;
  daemon-stop) daemon_stop "$2" ;;
  status) status ;;
  *) echo "usage: $0 up|down|daemons|stop-daemons|daemon-start N|daemon-stop N|status" >&2; exit 2 ;;
esac
