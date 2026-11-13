#!/usr/bin/env bash
# ABE-backed VM disks, end to end and unprivileged:
#   unshare -Urnm → fake abe 0.4.0 HTTP API (python, volumes are files) →
#   stub `nvme` that fakes sysfs + /dev/disk/by-id links → stub `virsh` and
#   `qemu-img` → real `xdebbied serve` with a host whose `abe` points at
#   the fake.
#
# Phases (each a fresh fake + daemon under $WORK/<phase>):
#   thin      A1-A8 from 0.14 adapted to thin root clones, plus: two VMs
#             share one sealed base (one fill, two clones); endpoint
#             refresh after abe moved an export; five concurrent POSTs of
#             the same volume name make one abe call; a DELETE whose abe
#             call fails leaves deleting:true, refuses attach, and a second
#             DELETE finishes the job.
#   legacy    abe < 0.4 (no /version): full-copy roots, no names, logged once.
#   crash     the fake dies right after /seal: the VM still comes up on a
#             restarted abe with exactly one fill.
#   capacity  15 GiB pool: the first 10 GiB VM runs, the second stays
#             Pending ("abe capacity"), POST /v1/volumes for 10 GiB → 507.
#   noabe     a host without [[hosts]].abe: the VM stays Pending ("no abe").
#
# Usage: tests/abe/run.sh            (builds release binaries if missing)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# Newest build wins: a stale release/ next to a fresh debug/ is the usual trap.
newest() { ls -t "$ROOT/target/release/$1" "$ROOT/target/debug/$1" 2>/dev/null | head -1; }
XDEBBIED=${XDEBBIED:-$(newest xdebbied)}
XDCTL=${XDCTL:-$(newest xdctl)}
BASE=${WORK:-/tmp/xdebbie-abe}
IMG_BYTES=$((64 << 20))   # what the stub qemu-img reports as the image's virtual size

if [[ "${1:-}" != "--inner" ]]; then
  [[ -x "$XDEBBIED" && -x "$XDCTL" ]] || (cd "$ROOT" && cargo build --release --quiet)
  rm -rf "$BASE"; mkdir -p "$BASE"
  exec unshare -Urnm --propagation private "$0" --inner
fi

fail=0
pass() { echo "PASS $*"; }
flunk() { echo "FAIL $*" >&2; fail=1; }
j() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
dashed() { echo "$1" | sed -E 's/(.{8})(.{4})(.{4})(.{4})(.{12})/\1-\2-\3-\4-\5/'; }
API=http://127.0.0.1:7443/v1
# curl returning "<status> <body>" on one line
req() { local b; b=$(mktemp); curl -s -o "$b" -w '%{http_code}' "$@"; echo " $(cat "$b")"; rm -f "$b"; }
calls() { curl -s http://127.0.0.1:8000/_calls; }
base_id() { # id of the (only) base volume in the fake
  for f in "$WORK"/abe/*.json; do python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); m.get("base") and print(m["id"])' "$f"; done | head -1
}
wait_phase() { # id phase [tries]
  local p=""; for _ in $(seq 1 "${3:-100}"); do
    p=$($X vm describe "$1" 2>/dev/null | j 'd["phase"]' || echo "")
    [ "$p" = "$2" ] && break; sleep 0.2
  done; echo "$p"
}
wait_msg() { # id substring [tries]
  local m=""; for _ in $(seq 1 "${3:-50}"); do
    m=$($X vm describe "$1" 2>/dev/null | j 'd.get("message") or ""' || echo "")
    [[ "$m" == *"$2"* ]] && break; sleep 0.2
  done; echo "$m"
}

ip link set lo up
DPID=""; APID=""
stop_env() { kill $DPID $APID 2>/dev/null || true; wait $DPID $APID 2>/dev/null || true; DPID=""; APID=""; }
trap stop_env EXIT

start_fake() { # env assignments...
  env FAKE_ABE_DIR="$WORK/abe" "$@" python3 "$HERE/fake-abe.py" 8000 >>"$WORK/abe.log" 2>&1 &
  APID=$!
  for _ in $(seq 1 50); do curl -s http://127.0.0.1:8000/_calls >/dev/null 2>&1 && break; sleep 0.1; done
}

# start_env <phase> [NOABE=1] [FAKE_ABE_*=... for the fake]
start_env() {
  local phase=$1; shift
  local noabe=0; [[ "${1:-}" == NOABE=1 ]] && { noabe=1; shift; }
  WORK="$BASE/$phase"
  mkdir -p "$WORK/sys" "$WORK/byid" "$WORK/abe" "$WORK/bin" "$WORK/pool" "$WORK/doms"
  # the daemon looks for /dev/disk/by-id/nvme-uuid.<id>; own that directory
  mountpoint -q /dev/disk/by-id && umount /dev/disk/by-id
  mount --bind "$WORK/byid" /dev/disk/by-id
  for s in nvme qemu-img virsh; do
    sed "s#@WORK@#$WORK#g" "$HERE/stubs/$s" > "$WORK/bin/$s"
  done
  chmod +x "$WORK"/bin/*
  : > "$WORK/pool/img.qcow2"   # base image present in the pool
  start_fake "$@"
  sed "s#@WORK@#$WORK#g" "$HERE/xdebbied.toml.in" > "$WORK/xdebbied.toml"
  [ $noabe = 1 ] && sed -i '/^abe = /d' "$WORK/xdebbied.toml"
  FAKE_IMAGE_BYTES=$IMG_BYTES RUST_LOG=${RUST_LOG:-info} "$XDEBBIED" --config "$WORK/xdebbied.toml" serve >"$WORK/xdebbied.log" 2>&1 &
  DPID=$!
  X="$XDCTL --url http://127.0.0.1:7443"
  for _ in $(seq 1 150); do $X host ls >/dev/null 2>&1 && break; sleep 0.1; done
  $X host ls >/dev/null 2>&1 || { echo "daemon did not come up:"; cat "$WORK/xdebbied.log"; exit 1; }
  echo "=== phase $phase ($WORK)"
}

############################################################################
start_env thin FAKE_ABE_FAIL_FIRST=1 FAKE_ABE_DELETE_FAIL_ONCE=1

# --- A1: host carries its abe url
if grep -q '"abe": *"http://127.0.0.1:8000"' "$WORK/state.json"; then
  pass "A1: host h1 registered with abe=http://127.0.0.1:8000"
else
  flunk "A1: abe url missing from host record"
fi

# --- A2: abe-backed VM with a data disk; first allocation (the ?base= call) fails, retry succeeds
id=$($X vm create abe-vm --image img --cpus 1 --memory-mib 256 --disk-gib 2 --data-disk-gib 1 --storage abe --json | j 'd["id"]')
phase=$(wait_phase "$id" Running)
show=$($X vm describe "$id")
if [ "$phase" = Running ]; then
  pass "A2: abe-vm reached Running (first /configure returned 500, retried)"
else
  flunk "A2: phase=$phase msg=$(j 'd.get("message")' <<<"$show")"; tail -20 "$WORK/xdebbied.log"
fi

# --- A3: a sealed base + a root clone + a data volume; roles/sizes/addresses recorded
nvols=$(ls "$WORK/abe"/*.json 2>/dev/null | wc -l)
rid=$(j '[x for x in d["disks"] if x["role"]=="root"][0]["id"]' <<<"$show")
did=$(j '[x for x in d["disks"] if x["role"]=="data"][0]["id"]' <<<"$show")
bid=$(base_id)
if [ "$nvols" = 3 ] && j 'all([x["populated"] and x["traddr"]=="10.22.0.1" and x["server"]=="http://127.0.0.1:8000" for x in d["disks"]]) and {x["role"]:x["bytes"] for x in d["disks"]}=={"root":2<<30,"data":1<<30} and [x.get("origin") for x in d["disks"]]==["'"$bid"'", None]' <<<"$show" | grep -q True \
   && j 'd["base"]=="img" and d["sealed"] and d["bytes"]=='"$IMG_BYTES" < "$WORK/abe/$bid.json" | grep -q True \
   && j 'd["origin"]=="'"$bid"'" and d["name"]=="vm-'"$id"'-root"' < "$WORK/abe/$rid.json" | grep -q True \
   && j 'd["name"]=="vm-'"$id"'-data"' < "$WORK/abe/$did.json" | grep -q True; then
  pass "A3: sealed base (img, $((IMG_BYTES>>20)) MiB) + root clone 2 GiB (origin=base) + data 1 GiB, named vm-<uuid>-root/-data"
else
  flunk "A3: nvols=$nvols bid=$bid disks=$(j 'd["disks"]' <<<"$show")"
fi
if j 'all(json.load(open(f))["raw"] for f in __import__("glob").glob("'"$WORK"'/abe/*.json"))' <<<'{}' | grep -q True; then
  pass "A3b: volumes requested raw (no GPT from abe)"
else
  flunk "A3b: abe got non-raw configure calls"
fi

# --- A4: base connected on 44001 for the fill, then freed by the seal; clone takes 44001, data 44002
if grep -q "connect -t tcp -a 10.22.0.1 -s 44001 -n $bid --ctrl-loss-tmo -1" "$WORK/nvme.log" \
   && grep -q "connect -t tcp -a 10.22.0.1 -s 44001 -n $rid --ctrl-loss-tmo -1" "$WORK/nvme.log" \
   && grep -q "connect -t tcp -a 10.22.0.1 -s 44002 -n $did --ctrl-loss-tmo -1" "$WORK/nvme.log" \
   && grep -q "disconnect -n $bid" "$WORK/nvme.log" \
   && [ "$(grep -c '^connect' "$WORK/nvme.log")" = 3 ]; then
  pass "A4a: nvme connect once per volume (base, clone, data) with infinite ctrl-loss-tmo; base disconnected after the fill"
else
  flunk "A4a: nvme.log: $(cat "$WORK/nvme.log")"
fi
if grep -q "convert -n -f qcow2 -O raw -t none -T none $WORK/pool/img.qcow2 /dev/disk/by-id/nvme-uuid.$(dashed "$bid")" "$WORK/qemu-img.log" \
   && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ] \
   && grep -q "FILLED-FROM:$WORK/pool/img.qcow2" "$WORK/dev-$bid" && ! grep -q FILLED "$WORK/dev-$rid" && ! grep -q FILLED "$WORK/dev-$did"; then
  pass "A4b: the base was filled from the pool image exactly once; clone and data disk untouched"
else
  flunk "A4b: qemu-img.log: $(cat "$WORK/qemu-img.log" 2>/dev/null)"
fi

# --- A5: domain XML uses block devices at the by-id paths; no local root/data volumes
xml="$WORK/doms/abe-vm.xml"
if grep -q "<source dev='/dev/disk/by-id/nvme-uuid.$(dashed "$rid")'/>" "$xml" \
   && grep -q "<source dev='/dev/disk/by-id/nvme-uuid.$(dashed "$did")'/>" "$xml" \
   && ! grep -q "abe-vm.qcow2" "$xml" && ! ls "$WORK/pool" | grep -q -E '^abe-vm(\.qcow2|-data)'; then
  pass "A5: domain XML has by-id block disks; no local root/data volume was created"
else
  flunk "A5: $(grep -n 'source' "$xml"; ls "$WORK/pool")"
fi

# --- A6: host "reboot": sysfs and by-id links vanish, domain is off → re-attached before start, not refilled
rm -rf "$WORK/sys"/* "$WORK/byid"/* "$WORK/dev-$rid" "$WORK/dev-$did"
echo off > "$WORK/doms/abe-vm.state"
for _ in $(seq 1 50); do grep -q running "$WORK/doms/abe-vm.state" && break; sleep 0.2; done
if grep -q running "$WORK/doms/abe-vm.state" && [ "$(grep -c '^connect' "$WORK/nvme.log")" = 5 ] && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ]; then
  pass "A6: after losing the devices the VM was re-attached (2 more connects) and started; root not refilled"
else
  flunk "A6: state=$(cat "$WORK/doms/abe-vm.state") connects=$(grep -c '^connect' "$WORK/nvme.log") fills=$(wc -l < "$WORK/qemu-img.log")"
fi

# --- A7: a local-storage VM on the same host does not touch abe/nvme
before=$(wc -l < "$WORK/nvme.log")
lid=$($X vm create local-vm --image img --cpus 1 --memory-mib 256 --disk-gib 1 --json | j 'd["id"]')
wait_phase "$lid" Running 50 >/dev/null
if [ "$($X vm describe "$lid" | j 'd["phase"]')" = Running ] && [ "$(wc -l < "$WORK/nvme.log")" = "$before" ] \
   && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 3 ] && ls "$WORK/pool" | grep -q '^local-vm.qcow2$' \
   && [ "$($X vm describe "$lid" | j 'len(d.get("disks",[]))')" = 0 ]; then
  pass "A7: local VM provisioned the old way; abe untouched, no disks[] on its record"
else
  flunk "A7: $($X vm describe "$lid")"
fi

# --- S1: a second VM from the same image shares the sealed base: no new base, no fill, one more clone
id2=$($X vm create abe-vm2 --image img --cpus 1 --memory-mib 256 --disk-gib 3 --storage abe --json | j 'd["id"]')
phase=$(wait_phase "$id2" Running)
show2=$($X vm describe "$id2")
rid2=$(j 'd["disks"][0]["id"]' <<<"$show2")
c=$(calls)
if [ "$phase" = Running ] \
   && [ "$(j 'len([p for m,p in d if p.startswith("/configure") and "base=" in p])' <<<"$c")" = 3 ] \
   && [ "$(j 'len([p for m,p in d if p.startswith("/seal/")])' <<<"$c")" = 1 ] \
   && [ "$(j 'len([p for m,p in d if p.startswith("/clone/'"$bid"'")])' <<<"$c")" = 2 ] \
   && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ] \
   && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 4 ] \
   && j 'd["disks"][0]["origin"]=="'"$bid"'" and d["disks"][0]["populated"] and d["disks"][0]["bytes"]==3<<30' <<<"$show2" | grep -q True; then
  pass "S1: two VMs, one image: ?base= three times (one 500, two answered with the same base), one fill, one /seal, two /clone; both roots have origin=base and populated=true"
else
  flunk "S1: phase=$phase calls=$c show2=$(j 'd["disks"]' <<<"$show2") nvols=$(ls "$WORK/abe"/*.json | wc -l) fills=$(wc -l < "$WORK/qemu-img.log")"
fi

# --- S8: abe moved abe-vm2's root to another port while the VM was down and the host lost the controller
mark=$(wc -l < "$WORK/nvme.log")
oldport=$(j 'd["disks"][0]["port"]' <<<"$show2")
rm -rf "$WORK/byid/nvme-uuid.$(dashed "$rid2")" "$WORK/dev-$rid2"
for d in "$WORK/sys"/nvme*; do [ "$(cat "$d/subsysnqn")" = "$rid2" ] && rm -rf "$d"; done
echo off > "$WORK/doms/abe-vm2.state"
newport=$(curl -s "http://127.0.0.1:8000/_move/$rid2" | j 'd["port"]')
for _ in $(seq 1 50); do grep -q running "$WORK/doms/abe-vm2.state" && break; sleep 0.2; done
tail -n +$((mark + 1)) "$WORK/nvme.log" > "$WORK/nvme.tail"
if grep -q running "$WORK/doms/abe-vm2.state" && [ "$newport" != "$oldport" ] \
   && [ "$($X vm describe "$id2" | j 'd["disks"][0]["port"]')" = "$newport" ] \
   && [ "$(sed -n 1p "$WORK/nvme.tail")" = "disconnect -n $rid2" ] \
   && grep -q "connect -t tcp -a 10.22.0.1 -s $newport -n $rid2" "$WORK/nvme.tail" \
   && grep -q "abe moved $rid2 from 10.22.0.1:$oldport to 10.22.0.1:$newport; record updated" "$WORK/xdebbied.log"; then
  pass "S8: endpoint refresh: record follows the export to port $newport; stale controller dropped, then connected on the new port; warning logged"
else
  flunk "S8: newport=$newport port=$($X vm describe "$id2" | j 'd["disks"][0]["port"]') tail=$(cat "$WORK/nvme.tail")"
fi

# --- S6: five concurrent POST /v1/volumes with the same name: one abe call, one 201, four 200
pids=""
for i in 1 2 3 4 5; do
  req -X POST "$API/volumes" -H 'content-type: application/json' -d '{"name":"shared","bytes":1073741824}' > "$WORK/post.$i" &
  pids="$pids $!"
done
wait $pids
codes=$(cut -d' ' -f1 "$WORK"/post.[1-5] | sort | tr '\n' ' ')
vid=$(j 'd["id"]' < <(cut -d' ' -f2- "$WORK/post.1"))
if [ "$codes" = "200 200 200 200 201 " ] \
   && [ "$(j 'len([p for m,p in d if p.startswith("/configure") and "name=vol-shared" in p])' <<<"$(calls)")" = 1 ] \
   && [ "$($X volume ls --json 2>/dev/null | j 'len([v for v in d if v["name"]=="shared"])' || echo x)" = 1 ] \
   && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 5 ] \
   && j 'd["name"]=="vol-shared"' < "$WORK/abe/$vid.json" | grep -q True; then
  pass "S6: five concurrent creates of 'shared': one /configure?name=vol-shared, one 201 + four 200, one record"
else
  flunk "S6: codes='$codes' vid=$vid calls=$(calls | grep -o '/configure[^"]*')"
fi

# --- S7: DELETE whose abe call fails: 502, deleting:true, attach refused, second DELETE finishes
r1=$(req -X DELETE "$API/volumes/$vid")
v=$(req "$API/volumes/$vid")
r2=$(req -X POST "$API/volumes/$vid/attach" -H 'content-type: application/json' -d '{"vm":"'"$id"'"}')
r3=$(req -X DELETE "$API/volumes/$vid")
sleep 0.5
r4=$(req "$API/volumes/$vid")
if [[ "$r1" == 502* ]] && [[ "$v" == 200* ]] && j 'd["deleting"]==True' < <(cut -d' ' -f2- <<<"$v") | grep -q True \
   && [[ "$r2" == 409* ]] && [[ "$r2" == *"being deleted"* ]] && [[ "$r3" == 204* ]] && [[ "$r4" == 404* ]] \
   && [ ! -e "$WORK/abe/$vid.json" ]; then
  pass "S7: delete race: abe 500 → 502 and deleting:true; attach → 409; second DELETE → 204 and the volume is gone"
else
  flunk "S7: r1=$r1 v=$v r2=$r2 r3=$r3 r4=$r4"
fi

# --- A8: delete disconnects and removes both VMs' volumes; the sealed base stays
$X vm rm "$id" >/dev/null; $X vm rm "$id2" >/dev/null
for _ in $(seq 1 50); do $X vm describe "$id" >/dev/null 2>&1 || $X vm describe "$id2" >/dev/null 2>&1 || break; sleep 0.2; done
if ! $X vm describe "$id" >/dev/null 2>&1 && ! $X vm describe "$id2" >/dev/null 2>&1 \
   && [ "$(ls "$WORK/abe"/*.json 2>/dev/null | wc -l)" = 1 ] && [ -e "$WORK/abe/$bid.json" ] \
   && grep -q "disconnect -n $rid" "$WORK/nvme.log" && grep -q "disconnect -n $did" "$WORK/nvme.log" && grep -q "disconnect -n $rid2" "$WORK/nvme.log" \
   && [ -z "$(ls "$WORK/sys")" ] && [ -z "$(ls "$WORK/byid")" ]; then
  pass "A8: delete disconnected and removed the clones and the data volume; the sealed base is kept"
else
  flunk "A8: abe vols=$(ls "$WORK/abe"); nvme.log: $(cat "$WORK/nvme.log")"
fi
stop_env

############################################################################
start_env legacy FAKE_ABE_LEGACY=1
id=$($X vm create old-vm --image img --cpus 1 --memory-mib 256 --disk-gib 2 --data-disk-gib 1 --storage abe --json | j 'd["id"]')
phase=$(wait_phase "$id" Running)
show=$($X vm describe "$id")
rid=$(j '[x for x in d["disks"] if x["role"]=="root"][0]["id"]' <<<"$show")
c=$(calls)
if [ "$phase" = Running ] && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 2 ] \
   && grep -q "convert -n -f qcow2 -O raw -t none -T none $WORK/pool/img.qcow2 /dev/disk/by-id/nvme-uuid.$(dashed "$rid")" "$WORK/qemu-img.log" \
   && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ] \
   && [ "$(j 'len([p for m,p in d if p.startswith(("/seal/","/clone/")) or "name=" in p or "base=" in p])' <<<"$c")" = 0 ] \
   && [ "$(j 'len([p for m,p in d if p=="/version"])' <<<"$c")" -ge 1 ] \
   && j 'all(x["populated"] and x.get("origin") is None for x in d["disks"])' <<<"$show" | grep -q True \
   && [ "$(grep -c 'predates 0.4.0: no thin root clones, no name idempotency' "$WORK/xdebbied.log")" = 1 ]; then
  pass "L1: legacy abe (no /version): root fully copied, no name/base/clone calls, no origin, warned once"
else
  flunk "L1: phase=$phase calls=$c disks=$(j 'd["disks"]' <<<"$show") log=$(grep -c predates "$WORK/xdebbied.log") hostls=$($X host ls | tail -1)"
fi
stop_env

############################################################################
start_env crash FAKE_ABE_DIE_AFTER=seal
id=$($X vm create crash-vm --image img --cpus 1 --memory-mib 256 --disk-gib 2 --storage abe --json | j 'd["id"]')
for _ in $(seq 1 100); do kill -0 $APID 2>/dev/null || break; sleep 0.2; done
if kill -0 $APID 2>/dev/null; then
  flunk "C1: the fake did not die after /seal"
else
  wait $APID 2>/dev/null || true
  start_fake   # same directory: the sealed base survives the restart
  phase=$(wait_phase "$id" Running)
  show=$($X vm describe "$id")
  bid=$(base_id)
  if [ "$phase" = Running ] && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 2 ] \
     && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ] \
     && j 'd["sealed"]' < "$WORK/abe/$bid.json" | grep -q True \
     && j 'd["disks"][0]["origin"]=="'"$bid"'" and d["disks"][0]["populated"]' <<<"$show" | grep -q True; then
    pass "C1: abe died after /seal; after its restart the VM was cloned from the sealed base with one fill"
  else
    flunk "C1: phase=$phase show=$show vols=$(ls "$WORK/abe") log=$(tail -5 "$WORK/xdebbied.log")"
  fi
fi
stop_env

############################################################################
start_env capacity FAKE_ABE_POOL_BYTES=$((15 << 30))
id=$($X vm create cap-1 --image img --cpus 1 --memory-mib 256 --disk-gib 10 --storage abe --json | j 'd["id"]')
phase=$(wait_phase "$id" Running)
sleep 2.5   # let the inventory see the new provisioned bytes
id2=$($X vm create cap-2 --image img --cpus 1 --memory-mib 256 --disk-gib 10 --storage abe --json | j 'd["id"]')
msg=$(wait_msg "$id2" "abe capacity")
phase2=$($X vm describe "$id2" | j 'd["phase"]')
r=$(req -X POST "$API/volumes" -H 'content-type: application/json' -d '{"name":"big","bytes":10737418240}')
r2=$(req -X POST "$API/volumes" -H 'content-type: application/json' -d '{"name":"small","bytes":1073741824}')
hl=$($X host ls)
if [ "$phase" = Running ] && [ "$phase2" = Pending ] && [[ "$msg" == *"abe capacity"* ]] \
   && [[ "$r" == 507* ]] && [[ "$r" == *"no ABE host fits 10737418240 bytes: h1: abe capacity"* ]] \
   && [[ "$r2" == 201* ]] && grep -E -q 'h1 .* [0-9]+/15 ' <<<"$hl" && $X host describe h1 | grep -q '"abeCapacity"'; then
  pass "K1: 15 GiB pool: cap-1 (10 GiB) runs, cap-2 stays Pending ($msg), 10 GiB volume → 507, 1 GiB volume fits; host ls shows ABE GiB"
else
  flunk "K1: phase=$phase phase2=$phase2 msg='$msg' r=$r r2=$r2 hostls=$(tail -1 <<<"$hl")"
fi
stop_env

############################################################################
start_env noabe NOABE=1
id=$($X vm create nowhere --image img --cpus 1 --memory-mib 256 --disk-gib 1 --storage abe --json | j 'd["id"]')
msg=$(wait_msg "$id" "no abe")
r=$(req -X POST "$API/volumes" -H 'content-type: application/json' -d '{"name":"v","bytes":1073741824}')
if [ "$($X vm describe "$id" | j 'd["phase"]')" = Pending ] && [[ "$msg" == *"no abe"* ]] && [[ "$r" == 503* ]] \
   && $X host ls | grep -E -q 'h1 .* - +Ready'; then
  pass "N1: no host with abe: the VM stays Pending ($msg); POST /v1/volumes → 503; host ls shows - for ABE"
else
  flunk "N1: msg='$msg' r=$r phase=$($X vm describe "$id" | j 'd["phase"]')"
fi
stop_env

echo
[ $fail = 0 ] && echo "all passed" || { echo "--- logs under $BASE/*/xdebbied.log"; exit 1; }
