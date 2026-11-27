# The mesh: underlay, overlay, bridge

xdebbied joins every box's VM bridge into one encrypted L2 network:
Geneve tunnels between the bridges, inside a WireGuard underlay it runs
itself.

The mesh is on by default. A box with no peers still pins the bridge MTU,
installs its nftables table and creates the WireGuard link, so it is ready
to be peered. `enabled = false` in `[mesh]` turns it off.

## What you need

- Two or more enrolled boxes ([Join a second box](../getting-started/second-box.md)).
- Each box reaches the others on UDP 51820, or is reached by them (a box
  behind NAT can leave its endpoint out).
- The `geneve` and `wireguard` kernel modules. The package loads both, and
  `/etc/modules-load.d/xdebbie-mesh.conf` loads them at boot.

## Add a peer

Peers are per box: each command changes only the box `xdctl` talks to.
`xdctl mesh` always talks to the box at `--url` (or `XDEBBIE_URL`), never
the leader, so run these on the box itself, or point `--url` at it.

1. Print this box's identity, on each box:

   ```bash
   xdctl mesh wg
   ```

2. On box A, add box B by its box id:

   ```bash
   xdctl mesh peer add 1 --pubkey BOX1_PUBKEY --endpoint box1.example.org:51820
   ```

3. On box B, add box A the same way.

4. Check, on both:

   ```bash
   xdctl mesh ls
   ```

To remove a peer: `xdctl mesh peer rm BOX` on each side. `peer add` with
a box that is already a peer replaces it. Each change is a read, modify,
`PUT /v1/mesh` with `If-Match`, so two edits at once cannot overwrite each
other.

## What you should see

`xdctl mesh ls` prints the peers, the WireGuard view, any clients, and the
conditions:

```text
BOX   DEVICE    REMOTE           PROBE            STATE         RTT  FDB  LAST CHANGE
1     gnv-b1    10.22.1.1        10.23.1.1        Up         0.40ms   17  3m

WIREGUARD wg-flock pubkey=... port=51820 address=10.22.0.1
...
CONDITIONS
  Ready                true  AllPeersUp
```

A peer is `Pending` until probed, then `Up` or `Down`. `Error` means a
kernel change for it failed; `reason` says what.

## The underlay: WireGuard

Geneve itself is plain text. Unless you set `allow_plain_underlay = true`,
xdebbied owns the underlay:

| What | Value |
|---|---|
| Link | `wg-flock` (`underlay`), kind wireguard, MTU 1420 (`[mesh.wireguard] mtu`) |
| Key | `/var/lib/xdebbie/wg.key`, made once, mode 0600. It survives teardown, so a box keeps its identity. |
| Address | `10.22.BOX.1/16` (`address`, `prefix`). The connected route sends `10.22.0.0/16` into the tunnel. |
| Port | UDP 51820 (`listen_port`) |
| Peers | one WireGuard peer per mesh peer: `allowed-ips 10.22.PEER.1/32`, its `endpoint` if given, keepalive 25 seconds (`keepalive`, `0` for off) |

With the managed underlay, a peer's `remote` is derived (`10.22.BOX.1`), so
a peer is just `{box, pubkey, endpoint}`. A peer behind NAT can leave
`endpoint` out: it calls the others and the keepalive holds the mapping.

If a box is renumbered, the mesh removes every other address from its own
WireGuard link and keeps only `10.22.BOX.1`.

## The overlay: Geneve

| What | Per | Detail |
|---|---|---|
| `gnv-bP` | peer P | Geneve link, VNI 23 (`vni`), UDP 6081 (`dstport`), `remote` the peer's underlay address, alias `xdebbie-mesh/v1`, a port of the bridge |
| Port flags | peer | `isolated on` (tunnels never bridge to each other, so no loops), `mcast_flood` and `bcast_flood` on; `learning` and `flood` off in `derived` mode, on in `learn` |
| Static FDB | peer, `derived` only | 17 entries: `52:54:00:23:PP:00` to `:0f` and `:fe` (the peer's bridge) |
| nftables | box | `table inet xdebbie_mesh` drops Geneve arriving on any interface but the underlay (`firewall = true`) |

VNI and port must be the same on every box.

## The bridge

The overlay bridge is the libvirt network `xdebbie`, bridge
`virbr-xdebbie`. On box N it has:

- the address `10.23.N.1/16`: the gateway and DNS its VMs are told, and
  the address the mesh probes in `derived` mode;
- the MAC `52:54:00:23:NN:fe` (NN in hex), which the other boxes' static
  FDB points at. Bootstrap sets it when it creates the network, from the
  box id the CA assigned;
- STP off. The mesh turns it off on every pass: isolated tunnel ports make
  the segment loop-free, and BPDUs across tunnels only confuse STP.

## FDB modes: derived and learn

`fdb = "derived"` (the default) installs static FDB entries for each peer's
bridge MAC and its 16 slot MACs, and the tunnels neither learn nor flood
unknown unicast. VM MACs, though, come from the VM's id (`52:54:00:` plus
the id's first three bytes), not from the slots. So in `derived` mode a VM
on box 0 may not reach a VM on box 1.

`fdb = "learn"` turns learning and flooding on for the tunnels: the bridges
learn VM MACs as they talk. If VMs on different boxes cannot reach each
other, set `fdb = "learn"` on every box. Changing it on one box only gives
you a mesh that works in one direction, which is a fun afternoon.

## MTU

`mtu = "auto"` makes the overlay MTU the underlay's minus 50 (outer IPv4 20,
UDP 8, Geneve 8, inner Ethernet 14): 1370 on a 1420 WireGuard link. A fixed
value must be at least 1280 and at most the underlay MTU minus 50; the
WireGuard MTU must then be at least 1330.

The MTU is applied to the tunnels, the bridge, and new VM NICs
(`<mtu size=.../>`). The DHCP responder also hands it out as option 26, so
guests that ignore cloud-init's network-config still get it.
`status.vms_mtu_mismatch` lists VM ports with another MTU (`xdctl mesh ls`
prints them under *VM NICs with stale MTU*); each is fixed when that VM is
re-created.

## Probes

Every peer is pinged every 1000 ms (`probe_interval_ms`), at its bridge
address in `derived` mode or its underlay address in `learn` mode (`probe`
overrides). A probe waits at most 800 ms (`probe_timeout_ms`), less once
RTTs are known: 15 times the smoothed RTT, at least 250 ms. Three misses in
a row make a peer `Down` (`probe_down_after`); three replies make it `Up`
(`probe_up_after`). The first probe goes out 1 second after start.

## Conditions

| Condition | Blocking | Meaning |
|---|---|---|
| `Ready` | | true when nothing blocks and every peer is `Up` (`AllPeersUp`) |
| `Degraded` | | some peer is not `Up` |
| `GeneveModuleMissing` | yes | `modprobe geneve` |
| `WireguardModuleMissing` | yes | `modprobe wireguard` |
| `WireguardConflict` | yes | the underlay name exists but is not a WireGuard link the mesh owns |
| `UnderlayMissing` | yes | the underlay interface does not exist (plain underlay) |
| `BridgeMissing` | yes | the overlay bridge does not exist |
| `BridgeAddressMismatch` | yes | the bridge holds an overlay address outside `10.23.BOX.0/24`, for example `10.23.0.1` on box 5: it would answer for another box |
| `BridgeMacMismatch` | no | the bridge MAC is not `52:54:00:23:BOX:fe`, so peers cannot reach this box's gateway address in `derived` mode |
| `BridgeStpEnabled` | no | turning STP off failed |
| `NameConflict` | no | something else owns a `gnv-bN` name or the underlay name |

A blocking condition stops every kernel change until it clears, but never
tears anything down. Peers also carry a `reason`, such as
`UnderlayRouteMismatch`: the route to `remote` leaves by another interface
than the underlay. The tunnel is then set down, not deleted.

## How it converges

The mesh reconciles every 5 seconds, or at once after a change: it reads
the kernel, plans the difference, and applies it. A restart on a converged
box makes no kernel changes at all. Each `ip`, `bridge` or `wg` command has
5 seconds before it is killed and retried on the next pass.

Ownership is strict. A link is the mesh's only if its name is `gnv-bN` (no
leading zeros, N up to 253) and its alias is `xdebbie-mesh/v1`. Anything
else, such as a stray `gnv-b9` with no alias, is never touched; it shows as
`NameConflict`. The only nftables table it touches is `inet xdebbie_mesh`.

The planned changes are counted by kind in `status.metrics`:
`create_tunnel`, `delete_tunnel`, `set_alias`, `set_mtu`, `set_master`,
`set_port_flags`, `set_up`, `set_down`, `fdb_add`, `fdb_del`,
`set_bridge_mtu`, `set_bridge_stp_off`, `wg_create`, `wg_set_iface`,
`wg_peer_set`, `wg_peer_del`, `addr_add`, `addr_del`, `delete_link`,
`blackhole_add`, `blackhole_del`.

## Where the spec comes from

Two sources, and the API wins:

1. `[mesh]` in `xdebbied.toml`. It is checked at startup; a bad section
   stops the daemon. No section means the defaults, with `bridge` taken
   from the first `[[hosts]]` entry.
2. `PUT /v1/mesh`, which every `xdctl mesh` change uses. It is kept in
   `/var/lib/xdebbie/mesh.json`, survives restarts, and shadows the TOML
   until `xdctl mesh delete` (`DELETE /v1/mesh`).

`xdctl mesh delete --yes` tears the mesh down and forgets the API spec;
`[mesh]` from the config then applies again. Teardown keeps the WireGuard
key and the [blackhole routes](routes-and-firewall.md#blackhole-routes-for-the-mesh-networks).

## Bring your own underlay

With `allow_plain_underlay = true` xdebbied does not manage WireGuard. You
give each peer a `remote` (its underlay address) and make the underlay
yourself. The route to each `remote` must leave by `underlay`, or the peer
reports `UnderlayRouteMismatch`. Clients need the managed underlay.

## Driving the mesh from another controller

A controller that manages peers itself calls its local daemon with
`PUT /v1/mesh` and `If-Match: GENERATION`, sending the whole peer list.
With the managed underlay each peer is `{box, pubkey, endpoint}`, and one
PUT adds or removes a peer: the daemon orders the WireGuard and Geneve
changes itself. Wait for `status.observed_generation == status.generation`
before treating a change as applied. Carry `spec.clients` over from the
`GET` unchanged, or the PUT removes every client.

## Mesh-only mode and tests

`xdebbied serve --mesh-only` runs just the API and the mesh: no libvirt, no
LVM, no discovery. The three-host harness in `tests/mesh/` uses it:

```bash
tests/mesh/run.sh                   # the acceptance tests, in network namespaces
ONLY="M4 M8" KEEP=1 tests/mesh/run.sh
```

## If it goes wrong

- If a peer is `Down` and `xdctl mesh wg` shows no handshake, then
  WireGuard cannot connect: check both sides have each other's key, the
  `endpoint`, and UDP 51820.
- If handshakes work but the peer stays `Down`, and `gnv-bN` TX climbs
  while the far side's RX stays 0, then the far box drops Geneve:
  `sudo ufw status` there must show `6081/udp on wg-flock ALLOW IN`.
- If boxes are `Up` but VMs on different boxes cannot reach each other,
  then set `fdb = "learn"` on every box.
- More: [The mesh is degraded](../reference/troubleshooting.md#the-mesh-is-degraded).

## Reference

[`[mesh]` keys](../reference/configuration.md#mesh),
[the mesh spec and status](../reference/mesh-spec.md),
[`xdctl mesh`](../reference/xdctl.md#mesh),
[`/v1/mesh`](../reference/api.md#mesh).
