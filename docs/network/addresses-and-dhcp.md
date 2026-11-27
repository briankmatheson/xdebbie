# Addresses and DHCP

Every VM gets a fixed overlay address when it is created, and xdebbied's
own DHCP responder hands it out. No lease database, no dynamic pool.

`[network]` in `xdebbied.toml` turns this on. Bootstrap writes it for every
box. Without it, VMs get no address from xdebbied and some other DHCP
server must serve the bridge.

## How a VM gets its address

1. **The MAC comes from the VM's id**: `52:54:00:` plus the first three
   bytes of its UUID.
2. **The address is picked at creation**, in the same store write as the VM
   record (`PUT /v1/vms/{id}`): the lowest free address in the pool, or the
   one you asked for with `--ip`. It shows in `xdctl vm ls` at once, before
   the VM has a host.
3. **The responder answers only MACs of VMs in its store**, with exactly
   the stored address, every time. An unknown MAC gets silence.
4. **The binding lives as long as the VM.** Deleting the VM frees the
   address; nothing expires. The lease time (24 hours by default) only sets
   how often guests renew, and renewals change nothing.

The pool is that of the box that created the VM, which is the leader. With
discovery off every box leads itself and allocates from its own `/24`. With
discovery on, one leader allocates for the whole cluster from its own pool,
so VMs keep leader addresses wherever they run, and the cluster holds at
most 253 VMs until you widen that box's pool. That is why every box forwards
for the whole overlay, not just its `/24` ([NAT](#nat-and-dns-for-vms)).

## The layout

One `/16` per cluster, one `/24` per box. Box N owns `10.23.N.0/24`: its
bridge is `.N.1` (the gateway and DNS its VMs are told), and its VMs get
`.N.2` to `.N.254`. Bootstrap writes exactly this: the first box from
`--subnet` (default `10.23.0.0/16`), a joining box from the CA box's
`GET /v1/network`. `cidr` and `domain` are the same on every box.

A narrower `cidr` holds fewer boxes: a `/24` box 0 only, a `/20` boxes
0-15. Prefixes longer than `/24` are refused by bootstrap.

```toml
[network]
cidr = "10.23.0.0/16"
gateway = "10.23.1.1"          # this box's bridge (box 1)
dns = ["10.23.1.1"]
pool_start = "10.23.1.2"
pool_end = "10.23.1.254"
reserved = []
upstream_dns = ["192.0.2.53"]  # where VM DNS to the gateway goes
# dhcp = true                  # false: allocate only, another server answers
# lease_secs = 86400
# domain = "vm.example"
# ntp = ["10.23.0.247"]
# leader_only = false
```

`--ip` may be any free host address in `cidr`, inside the pool or not:
pinning infrastructure outside the pool is what it is for.

## Pin an address

```bash
xdctl vm create web --image debian-13 --ip 10.23.0.5
```

It is refused if the address is taken, reserved, or named in a
[DNS-reserved zone](#reserve-addresses-through-dns), or if the box has no
`[network]`.

## What the guest is told

| Option | Name | Value | Sent |
|---|---|---|---|
| 1 | Subnet mask | from `cidr` | always |
| 3 | Router | `gateway` (default: first host of `cidr`) | always |
| 6 | DNS servers | `dns` (default: the gateway) | always |
| 12 | Host name | the VM's name | always |
| 15 | Domain name | `domain` | when set |
| 26 | Interface MTU | the mesh MTU | when the bridge is the mesh's bridge |
| 28 | Broadcast | from `cidr` | always |
| 42 | NTP servers | `ntp` | when not empty |
| 51 | Lease time | `lease_secs` (86400) | OFFER and ACK |
| 54 | Server id | this box's address on the bridge, else the gateway | always |
| 56 | Message | the reason | NAK only |
| 58 | Renewal (T1) | half of `lease_secs` | OFFER and ACK |
| 59 | Rebinding (T2) | seven eighths of `lease_secs` | OFFER and ACK |

| Guest sends | Responder answers |
|---|---|
| DISCOVER | OFFER with the stored address |
| REQUEST for the stored address | ACK |
| REQUEST for any other address | NAK, so a guest that remembers an old lease starts over |
| INFORM | ACK with options, no lease |
| anything, from an unknown MAC | nothing |

Replies are broadcast while the guest has no address, unicast to `ciaddr`
for renewals and INFORM, and sent to `giaddr` through a relay.

## Which box answers

Each box with `[network]` answers for the VMs in its own store
(`leader_only = false`, the default). Followers mirror the leader's records
every 5 seconds, so they give the same answers. With `leader_only = true`
only the elected leader answers; a follower keeps the socket open and stays
silent.

Once two boxes are meshed their bridges are one L2 segment, and every DHCP
server on either bridge hears every DISCOVER. Remove libvirt's dnsmasq
range or `isc-dhcp-server` from a bridge before you mesh it.

## Reserved addresses

`reserved = ["ADDRESS", ...]` keeps addresses with no name out of the pool.

### Reserve addresses through DNS

With `reserve_from_dns`, every A record inside `cidr` in the zones you name
is kept from VMs: a load balancer pool, a Kubernetes API endpoint, a hub's
own address. DNS becomes the one list to keep.

1. Allow zone transfers (AXFR over TCP) of the zone to each box's address.
   With BIND:

   ```text
   zone "mesh.example.org" {
       type primary;
       file "/etc/bind/db.mesh.example.org";
       allow-transfer { 192.0.2.10; 192.0.2.11; };
   };
   ```

2. Name the zones in `[network]`. `server` is an IP address, never a name
   (`IP:PORT` works too):

   ```toml
   reserve_from_dns = [
     { zone = "mesh.example.org", server = "10.23.0.11" },
   ]
   ```

3. Restart and check:

   ```bash
   sudo systemctl restart xdebbied
   xdctl net ls
   ```

`xdctl net ls` lists what DNS reserves and when each zone was last
transferred. Then:

- zones are read at start and every 60 seconds; a transfer may take 10
  seconds, 16 MiB and 4096 messages;
- a failed transfer keeps that zone's last good set, also saved in
  `/var/lib/xdebbie/dns-reserved.json`, so a box that starts while DNS is
  down keeps its reservations;
- a VM keeps an address it already has, even if DNS names it later; only
  new picks avoid reserved addresses;
- boxes that join later copy the setting with the rest of `[network]`; add
  it by hand on boxes that joined before.

## NTP for VMs

1. Set the servers in `[network]` on every box (IPv4 only):

   ```toml
   ntp = ["10.23.0.247", "10.23.1.247"]
   ```

2. Restart: `sudo systemctl restart xdebbied`. `xdctl net ls` shows them.

3. New VMs get option 42 at first boot. Running guests get it at their next
   renewal, half the lease time (12 hours by default), or now:

   ```bash
   sudo networkctl renew INTERFACE
   timedatectl timesync-status
   ```

Guests on systemd-networkd hand the servers to systemd-timesyncd
(`UseNTP=` is on by default). Other DHCP clients may need telling. `ntp = []`
stops offering it.

## NAT and DNS for VMs

VM traffic to the outside leaves by the box the VM runs on. VM-to-VM
traffic between boxes is bridged over the mesh and never touches this.
Bootstrap sets it up:

- the libvirt network forwards in route mode (`<forward mode='route'/>`);
  `ip_forward` is on and pinned in `/etc/sysctl.d/90-xdebbie-forward.conf`;
- ufw forwards the overlay out of the bridge
  (`route allow in on virbr-xdebbie from 10.23.0.0/16`);
- the nftables table `ip xdebbie_host` masquerades the overlay behind
  whatever interface a packet leaves by, sends VM DNS (to the bridge
  address, port 53, UDP and TCP) to the first `upstream_dns`, and drops new
  flows into the bridge from any other interface. A LAN host with a route
  to the overlay still cannot open a connection to a VM.

`upstream_dns` is the first box's own resolver (from
`/run/systemd/resolve/resolv.conf`, else `/etc/resolv.conf`, skipping
loopback), copied to every box that joins, so every VM asks the same
server. Empty means no DNAT: VMs then have no DNS. To change it, edit it on
every box and run `sudo xdebbied bootstrap`, which rewrites the table.

```bash
sudo nft list table ip xdebbie_host   # counters per rule
sudo ufw status                       # "xdebbie vm egress ..." is the forward rule
```

## Check the responder

```bash
xdctl net ls     # settings, responder state, counters, DNS reservations
xdctl net seen   # MACs heard on the bridge and what each was told
```

`xdctl net ls` follows the leader, like most commands. With discovery on,
see a follower's responder with `GET /v1/network` on that box. The
states:

- `serving`: bound on the bridge, answering.
- `standby (not the leader)`: `leader_only = true` and this box lost the
  election.
- `NOT LISTENING: bind udp/67 ...`: another DHCP server holds the port.
  The responder retries every 5 seconds.
- `NOT LISTENING: bridge ... does not exist`: retried every 5 seconds; it
  also rebinds by itself when libvirt recreates the bridge.

Every answer is logged at `info` with the exchange (`DISCOVER→OFFER`,
`REQUEST→NAK`, ...), the MAC, the VM and the address. `net seen`
remembers the last 1024 MACs.

## Moving an existing host to xdebbie's DHCP

1. Add `[network]` with the bridge's subnet.
2. Stop whatever served DHCP there: `virsh net-edit` to drop the `<dhcp>`
   range (libvirt then stops dnsmasq), or disable `isc-dhcp-server`.
3. Restart xdebbied. It adopts every VM that has no address: a VM that
   already reported an address inside `cidr` keeps it, the rest are
   allocated. `journalctl -u xdebbied | grep adopted` lists them.

When bootstrap finds an existing libvirt network with a `<dhcp>` range, it
writes `[network]` commented out, since dnsmasq probably still runs there.

## What it does not do yet

- No dynamic pool: guests xdebbied did not create are ignored
  (`xdctl net seen` shows them).
- No DNS of its own. VM names are not registered anywhere; queries to the
  gateway go to `upstream_dns`.
- No IPv6.

## If it goes wrong

- If a VM gets no address and `net ls` says `serving`, then look for the
  VM's MAC in `xdctl net seen`; if it is absent, the VM never asked (look
  at its console).
- If a VM has an address but no DNS, then `upstream_dns` is empty.
- More: [A VM gets no address](../reference/troubleshooting.md#a-vm-gets-no-address).

## Reference

[`[network]` keys](../reference/configuration.md#network),
[`xdctl net`](../reference/xdctl.md#network),
[`GET /v1/network`](../reference/api.md#network).
