# Overlay addressing and the built-in DHCP responder

`[network]` in `xdebbied.toml` makes xdebbie the owner of VM addressing on
the overlay bridge. It replaces libvirt's dnsmasq range or a hand-run
`dhcpd`, and it is what makes a Geneve mesh of bridges safe: one subnet,
one answer per MAC, no lease database to keep in sync.

## How it works

1. **The MAC is already deterministic.** `Vm::mac()` is
   `52:54:00:` + the first three bytes of the VM UUID.
2. **The address is allocated when the VM is created**, by the scheduler, in
   the same store transaction as the VM record (`PUT /v1/vms/{id}`). It is
   the lowest free address in the pool, or `spec.ip` if the caller pinned
   one. It shows up as `vm.ip` and as the first `InternalIP` in
   `vm.addresses` immediately — before the VM has a host, let alone a lease.
   `xdctl vm ls` prints it.
3. **The responder answers only for MACs it finds in the store**, with exactly
   the stored address, every time. DISCOVER → OFFER, REQUEST for the right
   address → ACK, REQUEST for anything else → NAK (so a guest that remembers
   a lease from an old server starts over), INFORM → ACK with options,
   unknown MAC → silence. There is no dynamic pool.
4. **The binding lives as long as the VM record.** Deleting the VM frees the
   address; nothing expires on its own. Lease time (default 24 h) only
   controls how often guests renew, and renewals are no-ops.

Because the answer is a pure function of the VM records, any box holding
the same records would say the same thing. Which box answers is policy:

- `leader_only = false` (default): every box with `[network]` answers — for
  the VMs in *its* store. With today's independent control planes that is
  exactly right: timesink answers for timesink's VMs, gram for gram's, and
  they never overlap because they never hold the same MAC.
- `leader_only = true`: answer only while this process is the elected
  leader. Use this once control planes share one store (replicated state);
  a follower then keeps the socket open but stays silent, and takes over
  with the same answers the moment it wins the election.

## Options handed to guests

| option | value |
|---|---|
| 1 subnet mask / 28 broadcast | from `cidr` |
| 3 router | `gateway` (default: first host of `cidr`) |
| 6 DNS | `dns` (default: the gateway) |
| 12 hostname | the VM name |
| 15 domain | `domain`, if set |
| 26 interface MTU | the mesh MTU when the bridge is a mesh overlay — so even guests that ignore cloud-init's `network-config` get the right MTU |
| 51 / 58 / 59 | `lease_secs`, T1 = ½, T2 = ⅞ |
| 54 server id | this box's address on the bridge (detected), else the gateway |

Replies are broadcast while the client has no address (we do not inject
ARP entries for `yiaddr`), unicast to `ciaddr` on renewals and INFORMs, and
sent to `giaddr` when a relay is involved.

## Configuration

```toml
[network]
cidr = "10.23.0.0/16"
# gateway = "10.23.0.1"       # default: first host of cidr
# dns = ["10.23.0.1"]         # default: the gateway
# pool_start = "10.23.0.16"   # default: cidr + 16
# pool_end = "10.23.255.254"  # default: last host
# reserved = ["10.23.0.2"]    # other boxes' bridge addresses, appliances
# dhcp = true                 # false = allocate only, some other server answers
# bridge = "virbr-int"        # default: the mesh bridge, else the first host's
# lease_secs = 86400
# domain = "vm.example"
# leader_only = false
```

`cidr`, `gateway`, `dns` and `domain` describe the overlay and must be the
same on every meshed box. `pool_start`/`pool_end` are per box: with
independent control planes on one overlay, give each box a disjoint slice
(`10.23.1.0–10.23.1.255` on box 1, `10.23.2.0–…` on box 2, …) so two
allocators never pick the same address. `reserved` is where the other
boxes' bridge addresses go.

`spec.ip` may be any free host address in `cidr`, inside the pool or not —
pinning infrastructure addresses outside the pool is the intended use.

Config is validated at startup; a bad `[network]` stops the daemon with a
clear message.

## Operating it

```
xdctl net show      # config, whether the responder is bound/serving, counters
xdctl net seen      # MACs heard on the bridge and what they were told
xdctl vm create web --image debian-12-base --ip 10.23.0.5 ...
```

`GET /v1/network` returns the same as JSON.

`xdctl net show` states:

- `serving` — bound on the bridge, answering.
- `standby (not the leader)` — `leader_only` and we lost the election.
- `NOT LISTENING: bind udp/67 …` — almost always another DHCP server on the
  bridge (libvirt's dnsmasq if the network still has a `<dhcp>` range, or
  `dhcpd`). The responder retries every 5 s and binds as soon as the port
  is free.
- `NOT LISTENING: bridge … does not exist` — retried every 5 s, and the
  responder also rebinds by itself when libvirt recreates the bridge
  (`net-destroy`/`net-start` changes the ifindex).

Every answer is logged at `info`:
`dhcp: DISCOVER → OFFER mac=52:54:00:cd:f3:20 vm="mesh-g1" ip=10.23.0.142`.

## Migrating an existing host

1. Add `[network]` with the bridge's subnet.
2. Stop whatever served DHCP on that bridge: `virsh net-edit` to drop the
   `<dhcp>` range (libvirt then stops dnsmasq), or disable `isc-dhcp-server`.
3. Restart `xdebbied`. On start it **adopts** every VM that has no `ip`: a VM
   that already reported an `InternalIP` inside `cidr` keeps it (so running
   guests see no change at renewal), everything else is allocated.
   `journalctl -u xdebbied | grep adopted` lists what happened.

`xdebbied bootstrap` on a fresh host now creates the libvirt network
**without** a DHCP range and writes an active `[network]`. When bootstrap
adopts a pre-existing network it writes the block commented out, since that
network probably still runs dnsmasq.

## What this does not do (yet)

- No dynamic pool for guests xdebbie did not create. They are ignored; use
  `xdctl net seen` to spot them.
- No DNS. `dns` just names resolvers for the guests.
- No floating gateway. With one L2 across boxes, VMs route via one box's
  NAT; a leader-held gateway VIP is the natural follow-up.
- No replication of the VM store between control planes. Every request
  already goes through the leader (docs/autodiscovery.md), so the
  leader's store is the truth; a new leader starts from its own copy.
