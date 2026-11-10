#!/usr/bin/env bash
# T0 spike: three "hosts" (netns h0,h1,h2) on a shared LAN bridge, each with
# br-int and one Geneve device per peer, all sharing VNI 23 / dstport 6081.
# Run as: unshare -Urnm bash /tmp/t0-spike.sh
set -euo pipefail
export PATH=/usr/sbin:/sbin:$PATH
mount -t tmpfs tmpfs /run
ip link set lo up
ip link add lan type bridge
ip link set lan up

for N in 0 1 2; do
  ip netns add h$N
  ip link add ul0 netns h$N type veth peer name lan$N
  ip link set lan$N master lan up
  ip -n h$N link set lo up
  ip -n h$N addr add 10.22.$N.1/16 dev ul0
  ip -n h$N link set ul0 mtu 1420 up
  ip -n h$N link add br-int type bridge stp_state 0
  ip -n h$N link set br-int address 52:54:00:23:0$N:fe
  ip -n h$N addr add 10.23.$N.1/16 dev br-int
  ip -n h$N link set br-int up
done

for N in 0 1 2; do
  for P in 0 1 2; do
    [[ $P == $N ]] && continue
    ip -n h$N link add gnv-b$P type geneve id 23 remote 10.22.$P.1 dstport 6081 ttl 64 df set
    ip -n h$N link set gnv-b$P alias xdebbie-mesh/v1
    ip -n h$N link set gnv-b$P mtu 1370
    ip -n h$N link set gnv-b$P master br-int
    ip netns exec h$N bridge link set dev gnv-b$P isolated on learning on flood on mcast_flood on bcast_flood on neigh_suppress off
    ip -n h$N link set gnv-b$P up
  done
  ip -n h$N link set br-int mtu 1370
done

echo "== h0 links =="; ip -n h0 -d link show type geneve | grep -E "^[0-9]+:|geneve"
echo "== h0 bridge ports =="; ip netns exec h0 bridge -d link show | grep -E "gnv|isolated"
echo "== ping matrix (host bridge addrs) =="
for N in 0 1 2; do for P in 0 1 2; do [[ $P == $N ]] && continue
  if ip netns exec h$N ping -c1 -W1 10.23.$P.1 >/dev/null; then echo "h$N -> 10.23.$P.1 ok"; else echo "h$N -> 10.23.$P.1 FAIL"; fi
done; done
echo "== MTU: 1342 payload must pass, 1343 must fail (DF) =="
ip netns exec h0 ping -c1 -W1 -M do -s 1342 10.23.1.1 >/dev/null && echo "1342 ok"
ip netns exec h0 ping -c1 -W1 -M do -s 1343 10.23.1.1 >/dev/null 2>&1 && echo "1343 UNEXPECTEDLY ok" || echo "1343 rejected (good)"
echo "== h0 fdb (learned on tunnels) =="; ip netns exec h0 bridge fdb show br br-int | grep gnv | grep -v permanent
echo "== static fdb replace test =="
ip netns exec h0 bridge fdb replace 52:54:00:23:01:05 dev gnv-b1 master static && ip netns exec h0 bridge fdb show br br-int | grep "01:05"
echo "== nft test =="
nft_test=$(ip netns exec h0 nft -f - <<'EOF' 2>&1 && echo nft-ok
table inet xdebbie_mesh {
  chain input {
    type filter hook input priority -10; policy accept;
    udp dport 6081 iifname != "ul0" counter drop
  }
}
EOF
); echo "$nft_test"
ip netns exec h0 nft -j list table inet xdebbie_mesh | head -c 300; echo
echo "T0 DONE"
