#!/usr/bin/env bash
# Geneve mesh acceptance tests M1–M13 (spec section 11.2) on the 3-host
# namespace harness. Prints PASS/FAIL per check; exit 1 if any failed.
#
#   sudo tests/mesh/run.sh            # as root
#   tests/mesh/run.sh                 # re-execs under `unshare -Urnm`
#
# Env: XDEBBIED / XDCTL (binaries), MESH_TEST_WORK (state dir),
#      KEEP=1 (leave namespaces up on exit), ONLY="M1 M5" (subset).
set -uo pipefail
export PATH=/usr/sbin:/sbin:$HOME/bin:$PATH   # ~/bin for an unprivileged `wg`

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
export XDEBBIED=${XDEBBIED:-$ROOT/target/debug/xdebbied}
export XDCTL=${XDCTL:-$ROOT/target/debug/xdctl}
export MESH_TEST_WORK=${MESH_TEST_WORK:-/tmp/xdebbie-mesh}
WORK=$MESH_TEST_WORK
NS=$HERE/netns-3host.sh

if [[ $(id -u) -ne 0 ]]; then
  if unshare -Urnm true 2>/dev/null; then
    echo "[run] not root: re-executing in an unprivileged user+net+mount namespace" >&2
    exec unshare -Urnm "$0" "$@"
  fi
  echo "need root or user namespaces" >&2
  exit 2
fi

[[ -x $XDEBBIED && -x $XDCTL ]] || { echo "build first: cargo build -p xdebbied -p xdctl" >&2; exit 2; }

PASS=0; FAIL=0; FAILED=()
pass() { echo "PASS $1: $2"; PASS=$((PASS+1)); }
fail() { echo "FAIL $1: $2"; FAIL=$((FAIL+1)); FAILED+=("$1"); }
check() { # check <id> <description> <command...>
  local id=$1 desc=$2; shift 2
  if "$@" >/dev/null 2>&1; then pass "$id" "$desc"; else fail "$id" "$desc"; fi
}
want() { [[ -z "${ONLY:-}" ]] || [[ " $ONLY " == *" $1 "* ]]; }

H() { ip netns exec h"$1" "${@:2}"; }                        # run in host N
V() { ip netns exec vm"$1" "${@:2}"; }                       # run in vm N
X() { ip netns exec h"$1" "$XDCTL" --url http://127.0.0.1:7443 "${@:2}"; }
J() { X "$1" mesh get; }                                     # JSON doc
jq_() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
pingok() { ip netns exec "$1" ping -c1 -W1 "$2" >/dev/null 2>&1; }

peer_state() { J "$1" | jq_ "[p['state'] for p in d['status']['peers'] if p['box']==$2][0]"; }
cond() { J "$1" | jq_ "[c['status'] for c in d['status']['conditions'] if c['type']=='$2'][0]"; }
ifindex() { ip -n h"$1" -j link show "$2" 2>/dev/null | jq_ "d[0]['ifindex']"; }
spec_json() { # full desired spec for host N (optionally dropping a peer)
  local N=$1 drop=${2:-99}
  local peers; peers=$(for P in 0 1 2; do [[ $P == "$N" || $P == "$drop" ]] || echo "{\"box\":$P,\"remote\":\"10.22.$P.1\"}"; done | paste -sd,)
  echo "{\"enabled\":true,\"box\":$N,\"networks\":[{\"bridge\":\"br-int\",\"vni\":23}],\"dstport\":6081,\"underlay\":\"ul0\",\"allow_plain_underlay\":true,\"fdb\":\"derived\",\"firewall\":true,\"peers\":[$peers]}"
}
apply() { spec_json "$1" "${2:-99}" | X "$1" mesh apply -f - >/dev/null; }
wait_for() { # wait_for <seconds> <command...>
  local t=$1; shift
  local end=$((SECONDS + t))
  while (( SECONDS < end )); do "$@" >/dev/null 2>&1 && return 0; sleep 0.5; done
  "$@" >/dev/null 2>&1
}
peer_is() { [[ $(peer_state "$1" "$2") == "$3" ]]; }
all_up() { [[ $(peer_state "$1" "$2") == Up && $(peer_state "$1" "$3") == Up && $(cond "$1" Ready) == True ]]; }
static_fdb_count() { # distinct (mac,dev) static entries on gnv-b* in host N
  H "$1" bridge -j fdb show br br-int | python3 -c "
import json,sys
s={(e['mac'],e['ifname']) for e in json.load(sys.stdin) if e.get('ifname','').startswith('gnv-b') and 'master' in e and e.get('state')=='static'}
print(len(s))"
}
actions_in_log() { grep -c 'action=' "$WORK/h$1/xdebbied.log" 2>/dev/null || true; }

cleanup() {
  if [[ "${KEEP:-0}" != 1 ]]; then "$NS" down >/dev/null 2>&1; fi
}
trap cleanup EXIT

echo "[run] bringing up 3-host harness"
"$NS" down >/dev/null 2>&1 || true
rm -rf "$WORK"
"$NS" up || exit 1
"$NS" daemons || exit 1
for N in 0 1 2; do apply $N || { echo "apply failed on h$N"; tail -5 "$WORK/h$N/xdebbied.log"; exit 1; }; done

# ---------------- M2 first: everything else assumes the mesh is Up ----------------
if want M2; then
  ok=1
  for N in 0 1 2; do
    others=($(for P in 0 1 2; do [[ $P == $N ]] || echo $P; done))
    wait_for 10 all_up $N "${others[0]}" "${others[1]}" || ok=0
  done
  if [[ $ok == 1 ]]; then pass M2 "all hosts show 2 peers Up and Ready within 10s"; else fail M2 "peers not Up/Ready in 10s"; X 0 mesh ls; fi
fi

# ---------------- M1 reachability ----------------
if want M1; then
  ok=1
  pingok vm0 10.23.1.11 || ok=0
  pingok vm0 10.23.2.11 || ok=0
  for A in 0 1 2; do for B in 0 1 2; do
    [[ $A == $B ]] && continue
    pingok vm$A 10.23.$B.11 || ok=0
    pingok vm$A 10.23.$B.1 || ok=0
  done; done
  [[ $ok == 1 ]] && pass M1 "every VM reaches every other VM and every host bridge address" || fail M1 "reachability"
fi

# ---------------- M3 kernel objects ----------------
if want M3; then
  ok=1; why=""
  for N in 0 1 2; do
    devs=$(ip -n h$N -j link show type geneve | jq_ "sorted(l['ifname'] for l in d)")
    exp=$(python3 -c "print(sorted('gnv-b%d'%p for p in (0,1,2) if p!=$N))")
    [[ "$devs" == "$exp" ]] || { ok=0; why+=" h$N devs=$devs"; }
    for P in 0 1 2; do [[ $P == $N ]] && continue
      l=$(ip -n h$N -j -d link show gnv-b$P)
      echo "$l" | jq_ "d[0]['ifalias']=='xdebbie-mesh/v1' and d[0]['master']=='br-int' and d[0]['mtu']==1370 and d[0]['linkinfo']['info_data']['id']==23 and d[0]['linkinfo']['info_data']['port']==6081 and d[0]['linkinfo']['info_data']['remote']=='10.22.$P.1'" | grep -q True || { ok=0; why+=" h$N/gnv-b$P link"; }
      H $N bridge -j -d link show dev gnv-b$P | jq_ "d[0]['isolated'] and not d[0]['learning'] and not d[0]['flood'] and d[0]['mcast_flood'] and d[0]['bcast_flood'] and not d[0]['neigh_suppress']" | grep -q True || { ok=0; why+=" h$N/gnv-b$P flags"; }
    done
    n=$(static_fdb_count $N); [[ "$n" == 34 ]] || { ok=0; why+=" h$N fdb=$n"; }
  done
  # STP: something (libvirt) turns it on → the next tick turns it back off.
  H 0 ip link set br-int type bridge stp_state 1
  X 0 mesh reconcile >/dev/null; sleep 1
  [[ $(ip -n h0 -j -d link show br-int | jq_ "d[0]['linkinfo']['info_data']['stp_state']") == 0 ]] || { ok=0; why+=" stp not forced off"; }
  [[ $(cond 0 BridgeStpEnabled) == False ]] || { ok=0; why+=" BridgeStpEnabled still set"; }
  [[ $ok == 1 ]] && pass M3 "2 tunnels per host with alias/master/flags, 34 static FDB entries, STP forced off" || fail M3 "kernel objects:$why"
fi

# ---------------- M4 no loops ----------------
if want M4; then
  # Count ARP requests leaving vm0 and arriving at vm1/vm2 with netdev hooks.
  V 0 nft -f - <<'EOF'
table netdev m4 {
  counter arpreq {}
  chain out {
    type filter hook egress device eth0 priority 0;
    arp operation request arp daddr ip 10.23.0.250 counter name "arpreq"
  }
}
EOF
  for N in 1 2; do V $N nft -f - <<'EOF'
table netdev m4 {
  counter arpreq {}
  chain in {
    type filter hook ingress device eth0 priority 0;
    arp operation request arp daddr ip 10.23.0.250 counter name "arpreq"
  }
}
EOF
  done
  V 0 ip neigh flush dev eth0 >/dev/null 2>&1
  for N in 1 2; do V $N ip neigh flush dev eth0 >/dev/null 2>&1; done
  V 0 ping -c1 -W1 10.23.0.250 >/dev/null 2>&1 || true   # nobody answers: pure ARP who-has broadcasts
  sleep 4   # let the neighbour state machine finish all mcast_solicit retries before reading counters
  ctr() { V "$1" nft -j list counter netdev m4 arpreq | jq_ "[e for e in d['nftables'] if 'counter' in e][0]['counter']['packets']"; }
  sent=$(ctr 0); got1=$(ctr 1); got2=$(ctr 2)
  # Tunnel packet rate must stay near the probe rate (~1/s each way), not explode.
  rx0=$(cat /sys/class/net/gnv-b1/statistics/rx_packets 2>/dev/null || H 0 cat /sys/class/net/gnv-b1/statistics/rx_packets)
  sleep 3
  rx1=$(H 0 cat /sys/class/net/gnv-b1/statistics/rx_packets)
  delta=$((rx1 - rx0))
  for N in 0 1 2; do V $N nft delete table netdev m4; done
  if [[ "$sent" -gt 0 && "$got1" == "$sent" && "$got2" == "$sent" && $delta -lt 50 ]]; then
    pass M4 "ARP broadcast from vm0 seen exactly once at vm1 and vm2 ($sent); tunnel rx +$delta pkts/3s"
  else
    fail M4 "sent=$sent got1=$got1 got2=$got2 tunnel-rx-delta=$delta"
  fi
fi

# ---------------- M5 MTU ----------------
if want M5; then
  ok=1
  V 0 ping -c1 -W1 -M do -s 1342 10.23.1.11 >/dev/null 2>&1 || ok=0
  V 0 ping -c1 -W1 -M do -s 1343 10.23.1.11 >/dev/null 2>&1 && ok=0
  [[ $(ip -n h0 -j link show br-int | jq_ "d[0]['mtu']") == 1370 ]] || ok=0
  [[ $(ip -n h0 -j link show gnv-b1 | jq_ "d[0]['mtu']") == 1370 ]] || ok=0
  [[ $(J 0 | jq_ "d['status']['mtu']") == 1370 ]] || ok=0
  [[ $ok == 1 ]] && pass M5 "1342-byte payload passes, 1343 fails; bridge and tunnels at 1370" || fail M5 "mtu"
fi

# ---------------- M6 peer failure ----------------
if want M6; then
  H 0 nft -f - <<'EOF'
table inet m6break {
  chain in {
    type filter hook input priority -20;
    ip saddr 10.22.1.1 drop
  }
  chain out {
    type filter hook output priority -20;
    ip daddr 10.22.1.1 drop
  }
}
EOF
  ok=1; why=""
  wait_for 6 peer_is 0 1 Down || { ok=0; why+=" peer1 not Down in 6s ($(peer_state 0 1))"; }
  [[ $(cond 0 Degraded) == True ]] || { ok=0; why+=" not Degraded"; }
  [[ $(cond 0 Ready) == False ]] || { ok=0; why+=" still Ready"; }
  pingok vm0 10.23.2.11 || { ok=0; why+=" vm0->vm2 broken"; }
  pingok vm1 10.23.2.11 || { ok=0; why+=" vm1->vm2 broken"; }
  [[ $(peer_state 0 2) == Up ]] || { ok=0; why+=" peer2 not Up"; }
  H 0 nft delete table inet m6break
  wait_for 8 peer_is 0 1 Up || { ok=0; why+=" peer1 not back Up"; }
  [[ $ok == 1 ]] && pass M6 "h0 sees peer 1 Down within 5s and Degraded; h0↔h2, h1↔h2 fine; recovers to Up" || fail M6 "$why"
fi

# ---------------- M7 peer removal ----------------
if want M7; then
  idx1=$(ifindex 0 gnv-b1)
  X 0 mesh peer rm 2 >/dev/null
  ok=1; why=""
  ip -n h0 link show gnv-b2 >/dev/null 2>&1 && { ok=0; why+=" gnv-b2 still exists"; }
  H 0 bridge fdb show br br-int | grep -q "gnv-b2" && { ok=0; why+=" fdb entries for gnv-b2 remain"; }
  [[ $(ifindex 0 gnv-b1) == "$idx1" ]] || { ok=0; why+=" gnv-b1 ifindex changed"; }
  [[ $(static_fdb_count 0) == 17 ]] || { ok=0; why+=" fdb=$(static_fdb_count 0)"; }
  [[ $(J 0 | jq_ "len(d['status']['peers'])") == 1 ]] || { ok=0; why+=" status still lists peer 2"; }
  X 0 mesh peer add 2 10.22.2.1 >/dev/null
  wait_for 10 all_up 0 1 2 || { ok=0; why+=" peer 2 did not come back Up"; }
  [[ $ok == 1 ]] && pass M7 "removing peer 2 deletes gnv-b2 + its FDB only; peer 1 untouched" || fail M7 "$why"
fi

# ---------------- M8 restart, no churn ----------------
if want M8; then
  idx1=$(ifindex 0 gnv-b1); idx2=$(ifindex 0 gnv-b2)
  V 0 ping -i 0.2 -c 40 10.23.1.11 > "$WORK/m8-ping.txt" 2>&1 &
  pp=$!
  sleep 1
  "$NS" daemon-stop 0
  "$NS" daemon-start 0
  wait $pp
  loss=$(grep -o '[0-9]*% packet loss' "$WORK/m8-ping.txt" | grep -o '^[0-9]*')
  sleep 6   # one full reconcile tick after startup
  acts=$(actions_in_log 0)
  ok=1; why=""
  [[ $(ifindex 0 gnv-b1) == "$idx1" && $(ifindex 0 gnv-b2) == "$idx2" ]] || { ok=0; why+=" ifindex changed"; }
  [[ "${acts:-0}" == 0 ]] || { ok=0; why+=" $acts actions logged after restart"; }
  [[ "$loss" == 0 ]] || { ok=0; why+=" ${loss}% loss during restart"; }
  wait_for 10 all_up 0 1 2 || { ok=0; why+=" not Ready after restart"; }
  [[ $ok == 1 ]] && pass M8 "daemon restart in h0: same ifindexes, zero actions, 0% loss" || fail M8 "$why"
fi

# ---------------- M9 daemon down ----------------
if want M9; then
  "$NS" daemon-stop 0
  ok=1
  pingok vm0 10.23.1.11 || ok=0
  pingok vm2 10.23.0.11 || ok=0
  "$NS" daemon-start 0
  wait_for 10 all_up 0 1 2 || ok=0
  [[ $ok == 1 ]] && pass M9 "VM traffic across hosts keeps flowing with the h0 daemon stopped" || fail M9 "traffic broke with daemon down"
fi

# ---------------- M10 foreign safety ----------------
if want M10; then
  ip -n h0 link add gnv-b9 type geneve id 23 remote 10.22.9.1 dstport 6081
  H 0 nft add table inet m10foreign
  ok=1; why=""
  X 0 mesh reconcile >/dev/null; sleep 1
  ip -n h0 link show gnv-b9 >/dev/null 2>&1 || { ok=0; why+=" gnv-b9 removed by reconcile"; }
  H 0 nft list table inet m10foreign >/dev/null 2>&1 || { ok=0; why+=" foreign nft table removed by reconcile"; }
  X 0 mesh delete --yes >/dev/null
  ip -n h0 link show gnv-b9 >/dev/null 2>&1 || { ok=0; why+=" gnv-b9 removed by DELETE"; }
  H 0 nft list table inet m10foreign >/dev/null 2>&1 || { ok=0; why+=" foreign nft table removed by DELETE"; }
  ip -n h0 link show gnv-b1 >/dev/null 2>&1 && { ok=0; why+=" gnv-b1 survived DELETE"; }
  ip -n h0 link del gnv-b9; H 0 nft delete table inet m10foreign
  apply 0; wait_for 10 all_up 0 1 2 || { ok=0; why+=" mesh not back after re-apply"; }
  [[ $ok == 1 ]] && pass M10 "foreign gnv-b9 and unrelated nft table survive reconcile and DELETE" || fail M10 "$why"
fi

# ---------------- M11 underlay guard ----------------
if want M11; then
  # A "wg0" that exists but carries no route: the route to every peer goes via ul0.
  ip -n h0 link add wg0 type dummy; ip -n h0 link set wg0 up
  idx1=$(ifindex 0 gnv-b1)
  spec_json 0 | python3 -c "import json,sys; d=json.load(sys.stdin); d['underlay']='wg0'; d['allow_plain_underlay']=False; print(json.dumps(d))" | X 0 mesh apply -f - >/dev/null
  sleep 1
  ok=1; why=""
  [[ $(peer_state 0 1) == Error ]] || { ok=0; why+=" peer1=$(peer_state 0 1)"; }
  [[ $(J 0 | jq_ "[p['reason'] for p in d['status']['peers'] if p['box']==1][0]") == UnderlayRouteMismatch ]] || { ok=0; why+=" reason"; }
  ip -n h0 link show gnv-b1 >/dev/null 2>&1 || { ok=0; why+=" gnv-b1 deleted"; }
  ip -n h0 -j link show gnv-b1 | jq_ "'UP' in d[0]['flags']" | grep -q False || { ok=0; why+=" gnv-b1 still up"; }
  [[ $(ifindex 0 gnv-b1) == "$idx1" ]] || { ok=0; why+=" recreated"; }
  # The firewall now only admits Geneve on wg0, so the tunnel really is cut.
  pingok vm0 10.23.1.11 && { ok=0; why+=" traffic still flows with underlay mismatch"; }
  apply 0; ip -n h0 link del wg0
  wait_for 10 all_up 0 1 2 || { ok=0; why+=" not recovered"; }
  [[ $ok == 1 ]] && pass M11 "route via non-underlay → peer Error UnderlayRouteMismatch, tunnel down not deleted" || fail M11 "$why"
fi

# ---------------- M12 teardown ----------------
if want M12; then
  X 0 mesh delete --yes >/dev/null
  ok=1; why=""
  [[ -z $(ip -n h0 -j link show type geneve | jq_ "''.join(l['ifname'] for l in d)") ]] || { ok=0; why+=" gnv-b* remain"; }
  H 0 nft list table inet xdebbie_mesh >/dev/null 2>&1 && { ok=0; why+=" nft table remains"; }
  ip -n h0 link show br-int >/dev/null 2>&1 || { ok=0; why+=" br-int gone"; }
  ip -n h0 link show vnet0 >/dev/null 2>&1 || { ok=0; why+=" vnet0 gone"; }
  pingok vm0 10.23.0.1 || { ok=0; why+=" vm0 lost its bridge"; }
  [[ $(J 0 | jq_ "d['status']['source']") == config ]] || { ok=0; why+=" source not config"; }
  apply 0; wait_for 10 all_up 0 1 2 || { ok=0; why+=" not back"; }
  [[ $ok == 1 ]] && pass M12 "delete removes all gnv-b* and the nft table; br-int and VMs untouched" || fail M12 "$why"
fi

# ---------------- M13 firewall ----------------
if want M13; then
  before=$(H 0 nft -j list table inet xdebbie_mesh | python3 -c "import json,sys; print(sum(e['rule']['expr'][2]['counter']['packets'] for e in json.load(sys.stdin)['nftables'] if 'rule' in e))")
  # UDP 6081 arriving on br-int (not the underlay) from the VM side.
  for _ in 1 2 3; do V 0 bash -c 'echo x > /dev/udp/10.23.0.1/6081' 2>/dev/null; done
  sleep 0.5
  after=$(H 0 nft -j list table inet xdebbie_mesh | python3 -c "import json,sys; print(sum(e['rule']['expr'][2]['counter']['packets'] for e in json.load(sys.stdin)['nftables'] if 'rule' in e))")
  # ...while Geneve on the underlay keeps working.
  ok=1
  (( after - before >= 3 )) || ok=0
  pingok vm0 10.23.1.11 || ok=0
  [[ $ok == 1 ]] && pass M13 "UDP 6081 on a non-underlay interface is counted+dropped (+$((after-before))); mesh unaffected" || fail M13 "before=$before after=$after"
fi

# ---------------- M14 WireGuard underlay (0.5.0) ----------------
# The mesh builds wg-flock itself on top of the carrier. ul0 is re-addressed
# to 10.99.N.1 for the duration so the derived WireGuard addresses
# (10.22.N.1/16) don't collide with it.
if want M14 && command -v wg >/dev/null; then
  ok=1; why=""
  for N in 0 1 2; do H $N ip addr replace 10.99.$N.1/16 dev ul0; H $N ip addr del 10.22.$N.1/16 dev ul0; done
  wg_spec() { # wg_spec <N> [peer json...]
    local N=$1; shift
    local peers; peers=$(printf '%s\n' "$@" | paste -sd,)
    echo "{\"enabled\":true,\"box\":$N,\"networks\":[{\"bridge\":\"br-int\",\"vni\":23}],\"underlay\":\"wg-flock\",\"wireguard\":{},\"fdb\":\"derived\",\"firewall\":true,\"peers\":[${peers}]}"
  }
  # 1. enable with zero peers: each daemon generates its key and builds wg-flock
  for N in 0 1 2; do wg_spec $N | X $N mesh apply -f - >/dev/null || { ok=0; why+=" apply0-h$N"; }; done
  declare -A PK
  for N in 0 1 2; do PK[$N]=$(X $N mesh wg --json | jq_ "d['pubkey']"); [[ ${#PK[$N]} == 44 ]] || { ok=0; why+=" h$N pubkey=${PK[$N]}"; }; done
  for N in 0 1 2; do H $N wg show wg-flock public-key | grep -qx "${PK[$N]}" || { ok=0; why+=" h$N kernel pubkey differs"; }; done
  # 2. enrol: remote omitted (derived), endpoint on the carrier
  for N in 0 1 2; do
    args=(); for P in 0 1 2; do [[ $P == "$N" ]] || args+=("{\"box\":$P,\"pubkey\":\"${PK[$P]}\",\"endpoint\":\"10.99.$P.1:51820\"}"); done
    wg_spec $N "${args[@]}" | X $N mesh apply -f - >/dev/null || { ok=0; why+=" apply-h$N"; }
  done
  wait_for 15 all_up 0 1 2 || { ok=0; why+=" peers not Up: $(peer_state 0 1)/$(peer_state 0 2)"; }
  [[ $(J 0 | jq_ "[p['remote'] for p in d['spec']['peers'] if p['box']==1][0]") == 10.22.1.1 ]] || { ok=0; why+=" remote not derived"; }
  [[ $(J 0 | jq_ "d['status']['mtu']") == 1370 ]] || { ok=0; why+=" mtu=$(J 0 | jq_ "d['status']['mtu']")"; }
  # 3. VM traffic flows, and the carrier sees only WireGuard (no plaintext Geneve)
  H 1 nft -f - <<'EOF'
table netdev m14 {
  counter gnv {}
  counter wgudp {}
  chain in {
    type filter hook ingress device ul0 priority 0;
    udp dport 6081 counter name "gnv"
    udp dport 51820 counter name "wgudp"
  }
}
EOF
  for i in 1 2 3; do pingok vm0 10.23.1.11 || { ok=0; why+=" vm0->vm1 ping $i"; }; done
  pingok vm2 10.23.1.11 || { ok=0; why+=" vm2->vm1"; }
  gcount=$(H 1 nft -j list counter netdev m14 gnv | jq_ "[e for e in d['nftables'] if 'counter' in e][0]['counter']['packets']")
  wcount=$(H 1 nft -j list counter netdev m14 wgudp | jq_ "[e for e in d['nftables'] if 'counter' in e][0]['counter']['packets']")
  H 1 nft delete table netdev m14
  [[ $gcount == 0 ]] || { ok=0; why+=" plaintext geneve on carrier ($gcount pkts)"; }
  (( wcount > 0 )) || { ok=0; why+=" no wireguard packets seen"; }
  hs=$(X 0 mesh wg --json | jq_ "[p['handshake_age_s'] for p in d['peers'] if p['box']==1][0]")
  [[ $hs != None && $hs -lt 60 ]] || { ok=0; why+=" no handshake with peer 1 ($hs)"; }
  # 4. restart h0 daemon: zero actions, key reused, same pubkey
  wgidx=$(ifindex 0 wg-flock)
  $NS daemon-stop 0 >/dev/null; : > "$WORK/h0/xdebbied.log"; $NS daemon-start 0 >/dev/null
  wait_for 10 all_up 0 1 2 || { ok=0; why+=" not Ready after restart"; }
  acts=$(grep -c 'action=' "$WORK/h0/xdebbied.log" || true)
  [[ ${acts:-0} == 0 ]] || { ok=0; why+=" $acts actions after restart"; }
  [[ $(ifindex 0 wg-flock) == "$wgidx" ]] || { ok=0; why+=" wg-flock recreated"; }
  [[ $(X 0 mesh wg --json | jq_ "d['pubkey']") == "${PK[0]}" ]] || { ok=0; why+=" pubkey changed"; }
  # 5. drop peer 2 on h0: wg peer + tunnel removed, peer 1 untouched
  X 0 mesh peer rm 2 >/dev/null || { ok=0; why+=" peer rm"; }
  H 0 wg show wg-flock peers | grep -q "${PK[2]}" && { ok=0; why+=" wg peer 2 still present"; }
  H 0 wg show wg-flock peers | grep -q "${PK[1]}" || { ok=0; why+=" wg peer 1 removed"; }
  ip -n h0 link show gnv-b2 >/dev/null 2>&1 && { ok=0; why+=" gnv-b2 still present"; }
  # 6. delete: wg-flock gone, key file kept, ul0 untouched
  X 0 mesh delete --yes >/dev/null
  ip -n h0 link show wg-flock >/dev/null 2>&1 && { ok=0; why+=" wg-flock survived delete"; }
  [[ -s $WORK/h0/wg.key ]] || { ok=0; why+=" wg.key removed"; }
  ip -n h0 link show ul0 >/dev/null 2>&1 || { ok=0; why+=" ul0 gone"; }
  [[ $ok == 1 ]] && pass M14 "WireGuard underlay: keys, enrolment, encrypted carrier ($wcount wg pkts, 0 geneve), silent restart, peer rm, teardown" || fail M14 "$why"
  # restore the plain-underlay world for anything that runs after
  for N in 1 2; do X $N mesh delete --yes >/dev/null 2>&1; done
  for N in 0 1 2; do H $N ip addr replace 10.22.$N.1/16 dev ul0; H $N ip addr del 10.99.$N.1/16 dev ul0 2>/dev/null; done
elif want M14; then
  fail M14 "wg not on PATH (apt install wireguard-tools, or drop wg into ~/bin)"
fi

echo
echo "[run] $PASS passed, $FAIL failed${FAILED[*]:+: ${FAILED[*]}}"
[[ $FAIL == 0 ]]
