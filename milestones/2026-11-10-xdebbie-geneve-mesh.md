# Milestone: xdebbie Geneve mesh (X12)

Spec: `XDebbie Geneve Mesh — Implementation Spec` v1.0 (2026-11-10).
Target: xdebbied 0.4.0-1.

## T0 — spike (2026-11-10)

Dev host has no passwordless sudo, so the spike ran in an unprivileged
user+net+mount namespace (`unshare -Urnm`). Everything the mesh needs
(geneve, bridge, veth, nft) works there; the `geneve` module was
auto-loaded by the first `ip link add ... type geneve`.

Result: **three Geneve devices per host with the same VNI (23) and dstport
(6081) but different remotes coexist**, so one-netdev-per-peer is viable.

Exact commands (per host N in 0..2, peers P != N):

```sh
ip netns add h$N
ip link add ul0 netns h$N type veth peer name lan$N
ip link set lan$N master lan up                    # "lan" is a bridge in the root netns
ip -n h$N addr add 10.22.$N.1/16 dev ul0
ip -n h$N link set ul0 mtu 1420 up
ip -n h$N link add br-int type bridge stp_state 0
ip -n h$N link set br-int address 52:54:00:23:0$N:fe
ip -n h$N addr add 10.23.$N.1/16 dev br-int
ip -n h$N link set br-int up

ip -n h$N link add gnv-b$P type geneve id 23 remote 10.22.$P.1 dstport 6081 ttl 64 df set
ip -n h$N link set gnv-b$P alias xdebbie-mesh/v1
ip -n h$N link set gnv-b$P mtu 1370
ip -n h$N link set gnv-b$P master br-int
ip netns exec h$N bridge link set dev gnv-b$P isolated on learning on flood on mcast_flood on bcast_flood on neigh_suppress off
ip -n h$N link set gnv-b$P up
ip -n h$N link set br-int mtu 1370
```

Observed:

- Full ping matrix between the three `br-int` addresses: OK.
- `ping -M do -s 1342` passes, `-s 1343` is rejected (DF honoured, MTU 1370 correct for a 1420 underlay).
- `bridge fdb replace <mac> dev gnv-b1 master static` works. Note: with the
  bridge's default `default_pvid 1`, `bridge fdb show` lists the entry twice
  (once with `vlan 1`, once without). Count static entries by distinct
  `(mac, dev)`, not by line.
- `bridge -d link show` reports `isolated on`; `ip -d link` reports
  `geneve id 23 remote 10.22.P.1 ttl 64 df set dstport 6081`.
- `nft -f -` creating `table inet xdebbie_mesh` with the dport-6081 drop
  rule works inside the namespace; `nft -j list table` returns JSON.

Script: `tests/mesh/t0-spike.sh` (kept for reference).

## Fix log

Things that bit during T1–T10, in the order they were found.

| Where | Symptom | Fix |
|---|---|---|
| `plan.rs` | `remote_change_recreates_only_that_peer` planned 0 FDB adds instead of 17: when a tunnel is recreated its FDB entries vanish with the device, but the planner still counted them as present | track a `recreated` set and exclude those devs from the "present" FDB map |
| `kernel.rs` | after convergence h0 logged 2 `fdb_del` per tick: the kernel's own `permanent` port-MAC entries on `gnv-b*` were treated as mesh-owned | `parse_fdb` sets `is_static` only for `state == "static"`; `permanent` entries are the kernel's |
| harness | `vnet0` showed up in `vms_mtu_mismatch` | set `vnet0` MTU 1370 in `netns-3host.sh` (libvirt does this via `<mtu size=…/>`) |
| `run.sh` M4/M6 | `nft -f` rejected one-line `table … { chain … { … } }` ("unexpected '}'") and `counter arp {}` ("unexpected arp": keyword) | multi-line heredocs; counter named `arpreq` |
| `run.sh` M4 | flaky `got2=3` vs `sent=2`: the neighbour state machine keeps sending `mcast_solicit` broadcasts ~1 s apart after `ping -W1` exits, so a third request slipped between reading the egress and ingress counters | count only `arp daddr ip 10.23.0.250` (the unowned probe target) and wait 4 s for solicitation to finish before reading; now deterministic (3/3/3) |
| `run.sh` M8 | `grep -c … \|\| echo 0` printed twice → "0\n0 actions" | `\|\| true` + `${acts:-0}` |
| `reconcile.rs` | pre-existing test didn't compile (`VmSpec` missing `data_disk_gib`) | added the field |
| `api/mod.rs` | `Router<App>: From<Router>` when merging the mesh router | call `.with_state()` before `.merge()`; auth is `from_fn_with_state(token, bearer_auth)` shared with `--mesh-only` |
| `xdctl` | `gen` is a reserved identifier in edition 2024; `serde_json::Value` ignores width formatting so the table was ragged | renamed; `.to_string()` the cells |
| T0 → design | `ip -j link` reports `df: "set"` and `bridge fdb` shows vlan-1 twins for every static entry | parser dedups by (mac, dev); twins are not diffed |
| packaging | `FakeNetOps` compiled into the release binary (dead-code warnings) | `#[cfg(test)]` |

## Results

### Unit tests (spec 11.1, `cargo test -p xdebbied`)

72 passed, 0 failed, 1 ignored (pre-existing). Mesh coverage: model (6),
validate (11), kernel parsers against captured fixtures, planner (11, incl.
idempotence: applying the plan to the fake kernel and re-diffing yields an
empty plan), firewall golden files (2), probe hysteresis (6), manager (9:
put→noop, idempotent put, If-Match/validation, restart adopts without
writes, delete, blocking conditions, route mismatch, config fallback, probe
events → Ready/Degraded), API handlers (4).

### Acceptance (spec 11.2, `tests/mesh/run.sh`, unprivileged `unshare -Urnm`, ~55 s)

Two consecutive full runs, 13/13 each:

```
PASS M1: every VM reaches every other VM and every host bridge address
PASS M2: all hosts show 2 peers Up and Ready within 10s
PASS M3: 2 tunnels per host with alias/master/flags, 34 static FDB entries
PASS M4: ARP broadcast from vm0 seen exactly once at vm1 and vm2 (3); tunnel rx +6 pkts/3s
PASS M5: 1342-byte payload passes, 1343 fails; bridge and tunnels at 1370
PASS M6: h0 sees peer 1 Down within 5s and Degraded; h0↔h2, h1↔h2 fine; recovers to Up
PASS M7: removing peer 2 deletes gnv-b2 + its FDB only; peer 1 untouched
PASS M8: daemon restart in h0: same ifindexes, zero actions, 0% loss
PASS M9: VM traffic across hosts keeps flowing with the h0 daemon stopped
PASS M10: foreign gnv-b9 and unrelated nft table survive reconcile and DELETE
PASS M11: route via non-underlay → peer Error UnderlayRouteMismatch, tunnel down not deleted
PASS M12: delete removes all gnv-b* and the nft table; br-int and VMs untouched
PASS M13: UDP 6081 on a non-underlay interface is counted+dropped (+3); mesh unaffected
[run] 13 passed, 0 failed
```

Zero-peer check (T10 acceptance): enabling the mesh on h0 with `peers: []`
and the bridge at 1500 logged exactly one action (`set_bridge_mtu br-int`),
created no `gnv-b*` links, and installed `table inet xdebbie_mesh`; status
`Ready=true`, `generation=observed_generation`.

### Packaging

`cargo deb -p xdebbied` → `xdebbied_0.4.0-1_amd64.deb`
(`Depends: … iproute2, nftables …`), ships
`/etc/modules-load.d/xdebbie-mesh.conf` (conffile) and
`usr/share/doc/xdebbie/mesh.md`; postinst `modprobe geneve`. The generated
`xdebbied.toml` has `[mesh]` commented out with `enabled = false`. The
systemd unit sets no `PrivateNetwork`/`RestrictAddressFamilies`/
`CapabilityBoundingSet`; `/var/lib/xdebbie` (home of `mesh.json`) is already
in `ReadWritePaths`.

Install on the dev host needs an interactive `sudo`:

```
sudo dpkg -i target/debian/xdebbied_0.4.0-1_amd64.deb
sudo systemctl restart xdebbied && xdctl mesh ls      # expect enabled=false
```

### Not done here

- 11.3 two-box test and T9 on real libvirt — done below under
  "0.5.2 — two-box test".

## Commits

```
fe30bba mesh: T0 spike — same-VNI Geneve devices per peer work (netns)
e677c0c mesh: model, validation, kernel ops, planner, firewall, probe, manager
8613a6d mesh: wire into daemon — [mesh] config, /v1/mesh API, --mesh-only, xdctl mesh
c0fcb54 mesh: 3-host netns harness and M1–M13 acceptance tests (T7/T8)
608177f mesh: packaging for 0.4.0-1 (T10)
```

## 0.5.0 — managed WireGuard underlay (2026-11-11)

The encryption layer moved into `xdebbied`: with `spec.wireguard` the mesh
builds `wg-flock` itself (key in `/var/lib/xdebbie/wg.key`, address
`10.22.<box>.1/16`, one WireGuard peer per mesh peer, `allowed-ips
<remote>/32`), and `peers[].remote` is derived. Same ownership/diff
discipline as the Geneve layer (`wg.rs` planner, fake kernel models the
connected route from the address).

- Unit tests: 86 (new: `wg show dump` + addr parsers against fixtures,
  5 planner tests incl. idempotence and foreign-link safety, 5 manager tests:
  build → silent restart with key reuse, peer removal + teardown keeps the
  key, foreign name / missing module block without writes, pubkey required,
  dropping `wireguard` from the spec removes only our link).
- Harness M14 (unprivileged netns, `wg` dropped into `~/bin` from the
  Debian package since the dev host lacks `wireguard-tools`): zero-peer
  enable generates keys and builds `wg-flock`; enrolment by pubkey+endpoint
  with derived remotes; peers Up, MTU 1370; nft ingress counters on the
  carrier show **0** UDP/6081 and >0 UDP/51820 while VMs ping across
  boxes; handshake age reported; h0 restart = zero actions, same ifindex,
  same pubkey; `peer rm` drops the WireGuard peer and tunnel; `delete`
  removes `wg-flock` but keeps `wg.key`. Full run: 14/14.
- Found while writing M14: `geneve` is an nft keyword too (counter names);
  `delete()` swapped in the config spec before tearing down, so the
  WireGuard device was orphaned → the manager now remembers the last
  managed device and tears down any *owned* link among current/last/config
  names.
- Package 0.5.0-1: `Depends` += `wireguard-tools`, modules-load.d adds
  `wireguard`, generated config carries a commented `[mesh.wireguard]`.

Two-box test: see "0.5.2 — two-box test" below.

## 0.5.1 / 0.5.2 — getting the two boxes talking (2026-11-11)

- 0.5.1: STP forced off on the mesh bridge every tick (`SetBridgeStpOff`),
  bootstrap writes `stp='off'`, `xdctl` resets SIGPIPE.
- 0.5.2: gram's reconcile failed every tick with
  `wg set wg-flock private-key /var/lib/xdebbie/wg.key … fopen: Permission
  denied` as root. Debian's AppArmor profile for `/usr/bin/wg` only allows
  keys under `/etc/wireguard`. Fix: xdebbied reads the key and pipes it to
  `wg set … private-key /dev/stdin` (pipes are not path-mediated); the .deb
  also ships `/etc/apparmor.d/local/wg` + `apparmor_parser -r` in postinst.
  Unit test with a stub `wg` asserts the key never appears on argv.
- Host firewall: ufw default-deny needs **two** rules — `51820/udp` on the
  carrier NIC *and* `6081/udp in on wg-flock` (decrypted Geneve hits INPUT a
  second time). Symptom without the second rule: handshake fine, `gnv-b<N>`
  TX climbing, far-side RX 0. Documented in `docs/mesh.md`.
- One L2 means one DHCP: gram's bootstrap-generated libvirt network had its
  own dnsmasq range (192.168.100.0/24). Redefined as `10.23.0.2/16`, no
  `<dhcp>`, `stp='off'`; timesink's `dhcpd` on `virbr-int` is now the only
  DHCP server on the segment. gram's VM took its lease from timesink's dhcpd
  across the mesh, default route via timesink's NAT, and `apt install iperf3`
  worked through it. Follow-up: leader-only built-in DHCP from the VM store
  (IP allocated at schedule time, option 26 = mesh MTU) — see 0.6.0 plan.

## 0.5.2 — two-box test (spec 11.3), 2026-11-11

Setup: timesink (box 0, `virbr-int` 10.23.0.1/16, enp2s0 192.168.1.157, kernel
6.12.115+deb13.1) ↔ gram (box 1, `virbr-xdebbie` 10.23.0.2/16, **wlo1 Wi-Fi**
192.168.1.162, kernel 7.3.3+deb14). Underlay `wg-flock` 10.22.0.1 ↔ 10.22.1.1,
Geneve VNI 23, bridge MTU 1370, `fdb = "learn"`. Both boxes `Ready true
AllPeersUp`, `BridgeStpEnabled false`, peer RTT 2.5–5.7 ms (Wi-Fi).

T9: new VMs created after the mesh came up got `vnet` MTU 1370 on both hosts
(`vnet5` on timesink, `vnet0` on gram) while pre-mesh VM NICs stayed at 1500
and are listed under "VM NICs with stale MTU". Inside the gram guest
(`mesh-g1`, debian-12-base, cloud-init netplan `mtu:`), `enp1s0` reports
`mtu 1370`.

Endpoints: `abe-01` on timesink (10.23.0.136, pre-mesh VM, NIC set to 1370 by
hand) ↔ `mesh-g1` on gram (10.23.0.142). `ping -M do -s 1342` (a full
1370-byte frame) passes without fragmentation.

```
iperf3, 10 s each, VM ↔ VM through Geneve-in-WireGuard
timesink → gram   TCP 1 stream   692 Mbit/s sender / 690 receiver   (1433 retr)
gram → timesink   TCP 1 stream   701 Mbit/s sender / 699 receiver   ( 283 retr)
timesink → gram   TCP 4 streams  718 Mbit/s sender / 711 receiver
timesink → gram   UDP 200 Mbit/s 196 Mbit/s, jitter 0.072 ms, 1.3 % loss
```

Tunnel accounting matches: WireGuard tx 2.12 GB / rx 0.99 GB on timesink and
the mirror on gram; `gnv-b1` rx 384 k pkts / `gnv-b0` rx 604 k pkts, zero
errors/drops. The ~700 Mbit/s ceiling and the retransmits are gram's Wi-Fi
link, not the overlay — the tunnel path (WireGuard + Geneve) ran at line rate
of the weakest hop.

Incidental: both LVM k8s base images on timesink (`debian-13-k8s-v1.36`,
`k8s-working`) loop at GRUB "Booting `Debian GNU/Linux'" under xdebbie's
BIOS/q35 domain — probably UEFI-only images; not a mesh issue. `debian-12-base`
needs importing into `timesink-vg` (`lvcreate` + `qemu-img convert`, needs
root) before a fresh timesink VM can be created from it.
