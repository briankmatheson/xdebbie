#!/usr/bin/env bash
# Built-in DHCP responder, end to end and unprivileged:
#   unshare -Urnm → bridge br-int 10.23.0.1/16 → real `xdebbied serve` with
#   [network] (no hosts, no libvirt) → VM created over the API gets an
#   address → busybox udhcpc on a veth with that VM's MAC receives exactly
#   that address plus mask/router/dns/hostname/mtu → a stranger MAC gets
#   nothing → a REQUEST for the wrong address is NAKed.
#
# Usage: tests/dhcp/run.sh            (builds release binaries if missing)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
XDEBBIED=${XDEBBIED:-$ROOT/target/release/xdebbied}
XDCTL=${XDCTL:-$ROOT/target/release/xdctl}
WORK=${WORK:-/tmp/xdebbie-dhcp}

if [[ "${1:-}" != "--inner" ]]; then
  [[ -x "$XDEBBIED" && -x "$XDCTL" ]] || (cd "$ROOT" && cargo build --release --quiet)
  rm -rf "$WORK"; mkdir -p "$WORK"
  exec unshare -Urnm --propagation private "$0" --inner
fi

export PATH=$PATH:/usr/sbin:/sbin
fail=0
pass() { echo "PASS $*"; }
flunk() { echo "FAIL $*" >&2; fail=1; }

ip link set lo up
# `ip netns` wants /run/netns; /run is read-only for us in a fresh userns.
if ! mkdir -p /run/netns 2>/dev/null; then mount -t tmpfs tmpfs /run; mkdir -p /run/netns; fi
ip link add br-int type bridge
ip addr add 10.23.0.1/16 dev br-int
ip link set br-int up
ip netns add cl
ip link add v0 type veth peer name c0
ip link set v0 master br-int up
ip link set c0 netns cl
ip -n cl link set lo up

cat > "$WORK/xdebbied.toml" <<EOF
listen = "127.0.0.1:7443"
state_file = "$WORK/state.json"
enable_discovery = false
virsh = "/bin/false"

[network]
cidr = "10.23.0.0/16"
dns = ["1.1.1.1", "8.8.8.8"]
pool_start = "10.23.0.100"
pool_end = "10.23.0.110"
reserved = ["10.23.0.2"]
bridge = "br-int"
domain = "vm.test"
lease_secs = 600
EOF

RUST_LOG=${RUST_LOG:-info,xdebbied::dhcp=debug} "$XDEBBIED" --config "$WORK/xdebbied.toml" serve \
  >"$WORK/xdebbied.log" 2>&1 &
DPID=$!
trap 'kill $DPID 2>/dev/null || true' EXIT
X="$XDCTL --url http://127.0.0.1:7443"
for _ in $(seq 1 50); do $X net show >/dev/null 2>&1 && break; sleep 0.1; done
$X net show >/dev/null 2>&1 || { echo "daemon did not come up:"; cat "$WORK/xdebbied.log"; exit 1; }

# --- D1: responder bound on the bridge, serving
for _ in $(seq 1 30); do $X net show | grep -q "dhcp: serving" && break; sleep 0.1; done
show=$($X net show)
if grep -q "dhcp: serving bridge=br-int server_id=10.23.0.1" <<<"$show"; then
  pass "D1: responder serving on br-int as 10.23.0.1"
else
  flunk "D1: $show"
fi

# --- D2: VM creation allocates the lowest free pool address and pins work
vm1=$($X vm create dhcp-one --image img --cpus 1 --memory-mib 256 --disk-gib 1 --json)
ip1=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["ip"])' <<<"$vm1")
id1=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$vm1")
vm2=$($X vm create dhcp-two --image img --cpus 1 --memory-mib 256 --disk-gib 1 --ip 10.23.0.7 --json)
ip2=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["ip"])' <<<"$vm2")
dup=$($X vm create dhcp-dup --image img --cpus 1 --memory-mib 256 --disk-gib 1 --ip 10.23.0.7 2>&1 || true)
res=$($X vm create dhcp-res --image img --cpus 1 --memory-mib 256 --disk-gib 1 --ip 10.23.0.2 2>&1 || true)
if [[ "$ip1" == "10.23.0.100" && "$ip2" == "10.23.0.7" ]] && grep -q "already bound" <<<"$dup" && grep -q "reserved" <<<"$res"; then
  pass "D2: allocation (lowest free $ip1, pinned $ip2, duplicate and reserved rejected)"
else
  flunk "D2: ip1=$ip1 ip2=$ip2 dup='$dup' res='$res'"
fi
mac1=$(python3 - "$id1" <<'EOF'
import sys,uuid
b=uuid.UUID(sys.argv[1]).bytes
print("52:54:00:%02x:%02x:%02x"%(b[0],b[1],b[2]))
EOF
)
ls_ip=$($X vm ls | awk '/dhcp-one/{print $5}')
[[ "$ls_ip" == "$ip1" ]] && pass "D2b: xdctl vm ls shows the allocated address before boot" || flunk "D2b: vm ls IP=$ls_ip"

# --- D3: a real client with the VM's MAC gets exactly that address + options
cat > "$WORK/udhcpc.sh" <<'EOF'
#!/bin/sh
[ "$1" = bound ] || [ "$1" = renew ] || exit 0
echo "event=$1 ip=$ip mask=$subnet router=$router dns=$dns hostname=$hostname mtu=$mtu domain=$domain serverid=$serverid lease=$lease" > "$OUT"
EOF
chmod +x "$WORK/udhcpc.sh"
client() { # mac iface-ns-cmd... ; writes $WORK/$1.out, returns udhcpc status
  local mac=$1 out=$WORK/$2.out; shift 2
  ip -n cl link set c0 address "$mac"
  ip -n cl link set c0 up
  : > "$out"
  OUT=$out ip netns exec cl busybox udhcpc -i c0 -f -n -q -t 2 -T 1 -s "$WORK/udhcpc.sh" "$@" >/dev/null 2>&1
}
if client "$mac1" one && grep -q "event=bound ip=$ip1 mask=255.255.0.0 router=10.23.0.1 dns=1.1.1.1 8.8.8.8 hostname=dhcp-one mtu=.* domain=vm.test serverid=10.23.0.1 lease=600" "$WORK/one.out"; then
  pass "D3: udhcpc with $mac1 was bound to $ip1 with mask/router/dns/hostname/domain/lease"
else
  flunk "D3: udhcpc output: $(cat "$WORK/one.out")"; tail -20 "$WORK/xdebbied.log"
fi
# no mesh → no option 26
grep -q "mtu= " "$WORK/one.out" && pass "D3b: no MTU option off the mesh" || flunk "D3b: $(cat "$WORK/one.out")"

# --- D4: a stranger gets nothing
if client 52:54:00:de:ad:01 stranger; then
  flunk "D4: unknown MAC got a lease: $(cat "$WORK/stranger.out")"
else
  pass "D4: unknown MAC 52:54:00:de:ad:01 got no lease"
fi

# --- D5: a client that remembers a stale address recovers (udhcpc -r sends
# DISCOVER+option 50, which we answer with the right address) ...
if client "$mac1" wrong -r 10.23.0.142 && grep -q "ip=$ip1 " "$WORK/wrong.out"; then
  pass "D5: client with a stale address ended on $ip1"
else
  flunk "D5: $(cat "$WORK/wrong.out")"
fi
# ... and a raw INIT-REBOOT REQUEST for the wrong address is NAKed. (A UDP
# socket only sees broadcasts on an interface that has an address; a client
# in INIT-REBOOT has one.)
ip -n cl link set c0 address "$mac1"
ip -n cl addr add 10.23.0.142/16 dev c0
nak=$(ip netns exec cl python3 "$HERE/nak.py" "$mac1" 10.23.0.142 10.23.0.1 2>&1 || true)
[[ "$nak" == "NAK" ]] && pass "D5b: raw REQUEST for 10.23.0.142 by $mac1 → NAK" || flunk "D5b: got '$nak'"
good=$(ip netns exec cl python3 "$HERE/nak.py" "$mac1" "$ip1" 10.23.0.1 2>&1 || true)
[[ "$good" == "ACK" ]] && pass "D5c: raw REQUEST for $ip1 → ACK" || flunk "D5c: got '$good'"
ip -n cl addr del 10.23.0.142/16 dev c0

# --- D6: counters + seen table
show=$($X net show); seen=$($X net seen)
if grep -Eq "offers=[2-9]|offers=[1-9][0-9]" <<<"$show" && grep -Eq "acks=[2-9]" <<<"$show" && grep -Eq "naks=[1-9]" <<<"$show" && grep -Eq "unknown_mac=[1-9]" <<<"$show" \
   && grep -q "$mac1 .*$ip1 .*dhcp-one" <<<"$seen" && grep -q "52:54:00:de:ad:01 .*unknown_mac" <<<"$seen"; then
  pass "D6: counters and seen table"
else
  flunk "D6: $show"$'\n'"$seen"
fi

# --- D7: deleting the VM frees the address for the next one
$X vm rm "$id1" >/dev/null
for _ in $(seq 1 50); do $X vm ls | grep -q dhcp-one || break; sleep 0.1; done
vm3=$($X vm create dhcp-three --image img --cpus 1 --memory-mib 256 --disk-gib 1 --json)
ip3=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["ip"])' <<<"$vm3")
[[ "$ip3" == "10.23.0.100" ]] && pass "D7: freed address $ip3 reused" || flunk "D7: ip3=$ip3"

# --- D8: bridge recreated → responder rebinds
ip link del br-int
sleep 0.5
ip link add br-int type bridge; ip addr add 10.23.0.1/16 dev br-int; ip link set br-int up; ip link set v0 master br-int up
mac3=$(python3 - "$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$vm3")" <<'EOF'
import sys,uuid
b=uuid.UUID(sys.argv[1]).bytes
print("52:54:00:%02x:%02x:%02x"%(b[0],b[1],b[2]))
EOF
)
ok=0
for _ in $(seq 1 10); do
  if client "$mac3" three && grep -q "ip=$ip3 " "$WORK/three.out"; then ok=1; break; fi
  sleep 1
done
[[ $ok == 1 ]] && pass "D8: responder rebound after the bridge was recreated" || { flunk "D8: $(cat "$WORK/three.out")"; tail -10 "$WORK/xdebbied.log"; }

if [[ $fail == 0 ]]; then echo "[dhcp] all passed"; else echo "[dhcp] FAILED"; fi
exit $fail
