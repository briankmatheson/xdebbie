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
gateway = "10.23.1.1"         # this box's bridge (box 1)
dns = ["10.23.1.1"]
pool_start = "10.23.1.2"
pool_end = "10.23.1.254"
reserved = []                 # appliances pinned by hand, if any
upstream_dns = ["192.168.1.1"]  # where VM DNS to the gateway is forwarded
# dhcp = true                 # false = allocate only, some other server answers
# bridge = "virbr-int"        # default: the mesh bridge, else the first host's
# lease_secs = 86400
# domain = "vm.example"
# leader_only = false
```

One /16 per overlay, one /24 per box. `cidr` (and `domain`) is the same on
every meshed box; box *N* owns `<cidr>.N.0/24`: its bridge is `.N.1` — the
gateway and DNS its VMs are told, and where the mesh probes it
(`docs/mesh.md`) — and its VMs get `.N.2`–`.N.254`. Pools are disjoint by
construction, so independent control planes on one overlay never pick the
same address and there is nothing to `reserved`. Bootstrap writes exactly
this for every box, the first one from `--subnet` and a joiner from the CA
box's `/v1/network` (BOOTSTRAP.md, "Joining a cluster"). A narrower `cidr`
just holds fewer boxes (a /24: box 0 only; a /20: boxes 0–15).

`spec.ip` may be any free host address in `cidr`, inside the pool or not —
pinning infrastructure addresses outside the pool is the intended use.

Config is validated at startup; a bad `[network]` stops the daemon with a
clear message.

## Leaving the box: NAT and DNS

Each box forwards for its own /24 and nothing else, so VM traffic leaves
by the box the VM runs on: a VM on timesink appears as timesink's address,
a VM on gram as gram's. VM-to-VM traffic between boxes is bridged over the
mesh and never touches this. Bootstrap sets it up (`bootstrap/firewall.rs`):

- the libvirt network is `<forward mode='route'/>` (an isolated network
  makes libvirt reject all forwarding; bootstrap warns, with the
  `virsh net-edit` to run, when it adopts one), `ip_forward` is on and
  pinned in `/etc/sysctl.d/90-xdebbie-forward.conf`;
- ufw forwards the /24 out of the bridge (`route allow in on <bridge>
  from <cidr>.N.0/24`);
- the `ip xdebbie_host` nftables table masquerades the /24 behind
  whatever interface a packet leaves by, DNATs the VMs' DNS — they are told
  the bridge, `.N.1`, port 53 udp and tcp — to `upstream_dns`, and drops new
  flows into the bridge from any other interface, so a LAN host with a
  route to the overlay still cannot open a connection to a VM.

`upstream_dns` is the first box's own resolver (from
`/run/systemd/resolve/resolv.conf`, else `/etc/resolv.conf`; loopback
skipped) and is copied to every joiner with the rest of `[network]`, so all
VMs in the cluster ask the same server box 0 does. Change it on box 0 and
re-join, or set it by hand everywhere. Empty = no DNAT: VM DNS goes nowhere.

```
sudo nft list table ip xdebbie_host     # counters per rule
sudo ufw status                         # "# xdebbie vm egress …" is the forward rule
```

## Operating it

```
xdctl net ls        # config, whether the responder is bound/serving, counters
xdctl net seen      # MACs heard on the bridge and what they were told
xdctl vm create web --image debian-12-base --ip 10.23.0.5 ...
```

`GET /v1/network` returns the same as JSON.

`xdctl net ls` states:

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
- No DNS of its own. `dns` names the bridge; queries there are forwarded to
  `upstream_dns`. VM names are not registered anywhere.
- No replication of the VM store between control planes. Every request
  already goes through the leader (docs/autodiscovery.md), so the
  leader's store is the truth; a new leader starts from its own copy.
