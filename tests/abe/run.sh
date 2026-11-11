#!/usr/bin/env bash
# ABE-backed VM disks, end to end and unprivileged:
#   unshare -Urnm → fake abe HTTP API (python, volumes are files) → stub
#   `nvme` that fakes sysfs + /dev/disk/by-id links → stub `virsh` → real
#   `xdebbied serve` with a host whose `abe` points at the fake → a VM
#   created with --storage abe gets a root + data volume allocated, attached,
#   root filled from the base image with (stub) qemu-img, domain XML uses the
#   by-id paths; a failed first allocation is retried without leaking; after
#   a simulated host reboot the disks are re-attached but not refilled;
#   delete disconnects and removes both volumes.
#
# Usage: tests/abe/run.sh            (builds release binaries if missing)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
XDEBBIED=${XDEBBIED:-$ROOT/target/release/xdebbied}
XDCTL=${XDCTL:-$ROOT/target/release/xdctl}
WORK=${WORK:-/tmp/xdebbie-abe}

if [[ "${1:-}" != "--inner" ]]; then
  [[ -x "$XDEBBIED" && -x "$XDCTL" ]] || (cd "$ROOT" && cargo build --release --quiet)
  rm -rf "$WORK"; mkdir -p "$WORK"
  exec unshare -Urnm --propagation private "$0" --inner
fi

fail=0
pass() { echo "PASS $*"; }
flunk() { echo "FAIL $*" >&2; fail=1; }
j() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
dashed() { echo "$1" | sed -E 's/(.{8})(.{4})(.{4})(.{4})(.{12})/\1-\2-\3-\4-\5/'; }

ip link set lo up
mkdir -p "$WORK/sys" "$WORK/byid" "$WORK/abe" "$WORK/bin" "$WORK/pool" "$WORK/doms"
# the daemon looks for /dev/disk/by-id/nvme-uuid.<id>; own that directory
mount --bind "$WORK/byid" /dev/disk/by-id

# stubs live in tests/abe/stubs/*; they read $WORK from the environment
for s in nvme qemu-img virsh; do
  sed "s#@WORK@#$WORK#g" "$HERE/stubs/$s" > "$WORK/bin/$s"
done
chmod +x "$WORK"/bin/*
: > "$WORK/pool/img.qcow2"   # base image present in the pool

# fake abe; first /configure fails to exercise the retry path
FAKE_ABE_DIR="$WORK/abe" FAKE_ABE_FAIL_FIRST=1 python3 "$HERE/fake-abe.py" 8000 >"$WORK/abe.log" 2>&1 &
APID=$!

sed "s#@WORK@#$WORK#g" "$HERE/xdebbied.toml.in" > "$WORK/xdebbied.toml"
RUST_LOG=${RUST_LOG:-info} "$XDEBBIED" --config "$WORK/xdebbied.toml" serve >"$WORK/xdebbied.log" 2>&1 &
DPID=$!
trap 'kill $DPID $APID 2>/dev/null || true' EXIT
X="$XDCTL --url http://127.0.0.1:7443"
for _ in $(seq 1 50); do $X host ls >/dev/null 2>&1 && break; sleep 0.1; done
$X host ls >/dev/null 2>&1 || { echo "daemon did not come up:"; cat "$WORK/xdebbied.log"; exit 1; }

# --- A1: host carries its abe url
if grep -q '"abe": *"http://127.0.0.1:8000"' "$WORK/state.json"; then
  pass "A1: host h1 registered with abe=http://127.0.0.1:8000"
else
  flunk "A1: abe url missing from host record"
fi

# --- A2: abe-backed VM with a data disk; first allocation fails (simulated), retry succeeds
vm=$($X vm create abe-vm --image img --cpus 1 --memory-mib 256 --disk-gib 2 --data-disk-gib 1 --storage abe --json)
id=$(j 'd["id"]' <<<"$vm")
phase=""
for _ in $(seq 1 100); do
  phase=$($X vm get "$id" --json 2>/dev/null | j 'd["phase"]' || echo "")
  [ "$phase" = Running ] && break; sleep 0.2
done
show=$($X vm get "$id" --json)
if [ "$phase" = Running ]; then
  pass "A2: abe-vm reached Running (first /configure returned 500, retried)"
else
  flunk "A2: phase=$phase msg=$(j 'd.get("message")' <<<"$show")"; tail -20 "$WORK/xdebbied.log"
fi

# --- A3: exactly two volumes exist, roles/sizes/addresses recorded
nvols=$(ls "$WORK/abe"/*.json 2>/dev/null | wc -l)
rid=$(j '[x for x in d["disks"] if x["role"]=="root"][0]["id"]' <<<"$show")
did=$(j '[x for x in d["disks"] if x["role"]=="data"][0]["id"]' <<<"$show")
if [ "$nvols" = 2 ] && j 'all([x["populated"] and x["traddr"]=="10.22.0.1" and x["server"]=="http://127.0.0.1:8000" for x in d["disks"]]) and {x["role"]:x["bytes"] for x in d["disks"]}=={"root":2<<30,"data":1<<30}' <<<"$show" | grep -q True; then
  pass "A3: root 2 GiB + data 1 GiB allocated once each with traddr/server; the failed call leaked nothing"
else
  flunk "A3: nvols=$nvols disks=$(j 'd["disks"]' <<<"$show")"
fi
if grep -q 'raw=true' "$WORK/abe.log" 2>/dev/null || grep -q 'raw=true' "$WORK/abe"/*.json 2>/dev/null; then :; fi
if j 'all(json.load(open(f))["raw"] for f in __import__("glob").glob("'"$WORK"'/abe/*.json"))' <<<'{}' | grep -q True; then
  pass "A3b: volumes requested raw (no GPT from abe)"
else
  flunk "A3b: abe got non-raw configure calls"
fi

# --- A4: attached with the right args; root filled from the pool base image via by-id path; data blank
if grep -q "connect -t tcp -a 10.22.0.1 -s 44001 -n $rid --ctrl-loss-tmo -1" "$WORK/nvme.log" \
   && grep -q "connect -t tcp -a 10.22.0.1 -s 44002 -n $did --ctrl-loss-tmo -1" "$WORK/nvme.log" \
   && [ "$(grep -c '^connect' "$WORK/nvme.log")" = 2 ]; then
  pass "A4a: nvme connect once per volume with infinite ctrl-loss-tmo"
else
  flunk "A4a: nvme.log: $(cat "$WORK/nvme.log")"
fi
if grep -q "convert -n -f qcow2 -O raw -t none -T none $WORK/pool/img.qcow2 /dev/disk/by-id/nvme-uuid.$(dashed "$rid")" "$WORK/qemu-img.log" \
   && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ] \
   && grep -q "FILLED-FROM:$WORK/pool/img.qcow2" "$WORK/dev-$rid" && ! grep -q FILLED "$WORK/dev-$did"; then
  pass "A4b: root filled from the pool's base image exactly once; data disk untouched"
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
if grep -q running "$WORK/doms/abe-vm.state" && [ "$(grep -c '^connect' "$WORK/nvme.log")" = 4 ] && [ "$(wc -l < "$WORK/qemu-img.log")" = 1 ]; then
  pass "A6: after losing the devices the VM was re-attached (2 more connects) and started; root not refilled"
else
  flunk "A6: state=$(cat "$WORK/doms/abe-vm.state") connects=$(grep -c '^connect' "$WORK/nvme.log") fills=$(wc -l < "$WORK/qemu-img.log")"
fi

# --- A7: a local-storage VM on the same host does not touch abe/nvme
before=$(wc -l < "$WORK/nvme.log")
lid=$($X vm create local-vm --image img --cpus 1 --memory-mib 256 --disk-gib 1 --json | j 'd["id"]')
for _ in $(seq 1 50); do [ "$($X vm get "$lid" --json | j 'd["phase"]')" = Running ] && break; sleep 0.2; done
if [ "$($X vm get "$lid" --json | j 'd["phase"]')" = Running ] && [ "$(wc -l < "$WORK/nvme.log")" = "$before" ] \
   && [ "$(ls "$WORK/abe"/*.json | wc -l)" = 2 ] && ls "$WORK/pool" | grep -q '^local-vm.qcow2$' \
   && [ "$($X vm get "$lid" --json | j 'len(d.get("disks",[]))')" = 0 ]; then
  pass "A7: local VM provisioned the old way; abe untouched, no disks[] on its record"
else
  flunk "A7: $($X vm get "$lid")"
fi

# --- A8: delete disconnects and removes both volumes
$X vm rm "$id" >/dev/null
for _ in $(seq 1 50); do $X vm get "$id" >/dev/null 2>&1 || break; sleep 0.2; done
if ! $X vm get "$id" >/dev/null 2>&1 && [ "$(ls "$WORK/abe"/*.json 2>/dev/null | wc -l)" = 0 ] \
   && grep -q "disconnect -n $rid" "$WORK/nvme.log" && grep -q "disconnect -n $did" "$WORK/nvme.log" \
   && [ -z "$(ls "$WORK/sys")" ] && [ -z "$(ls "$WORK/byid")" ]; then
  pass "A8: delete disconnected both volumes and removed them from abe"
else
  flunk "A8: abe vols=$(ls "$WORK/abe"); nvme.log: $(cat "$WORK/nvme.log")"
fi

echo
[ $fail = 0 ] && echo "all passed" || { echo "--- xdebbied.log"; tail -40 "$WORK/xdebbied.log"; exit 1; }
