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

(appended as work progresses)
