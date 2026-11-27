# Routes and firewall

What xdebbie lets into a box, what it drops, and the routes that keep
mesh traffic from leaking out the default route.

## What you need

Nothing to install: bootstrap sets all of this up, and the mesh keeps the
routes. This page is for knowing what is there and changing it.

## The host firewall (ufw)

Bootstrap owns the host firewall through ufw, with default deny inbound
and allow outbound. Every rule it adds carries a comment starting
`xdebbie`, and tagged rules it no longer wants are deleted, so re-running
`sudo xdebbied bootstrap` converges. It also removes the `XDEBBIE-INPUT`
iptables chain older versions made. Rules apply to IPv4 and IPv6 alike.

From the LAN, a box accepts only:

| Port | From | Rule comment |
|---|---|---|
| 22/tcp (SSH) | anywhere | `xdebbie ssh` |
| 51820/udp (WireGuard, `listen_port`) | anywhere | `xdebbie wireguard underlay` |
| 23443/tcp (the API, `listen`) | each network the box has an IPv4 address on | `xdebbie api from NET` |
| 23442/udp (discovery, `multicast_addr`) | the same networks, only while discovery is on | `xdebbie discovery from NET` |

Everything else is accepted on the mesh's own interfaces only:

| Port | On | Rule comment |
|---|---|---|
| 16514/tcp (libvirt TLS) | the underlay and the bridge | `xdebbie libvirt tls on IFACE` |
| 6081/udp (Geneve, `dstport`) | the underlay | `xdebbie geneve on wg-flock` |
| 67/udp (DHCP) | the bridge | `xdebbie dhcp on virbr-xdebbie` |
| 23000-23999/tcp and abe's API port | the storage bridge, from `storage_cidr` | `xdebbie abe ... on virbr-abe` |
| forward out of the bridge | from the overlay | `xdebbie vm egress ...` |
| forward from the underlay into the bridge | only with `mesh_routed` | `xdebbie mesh ... to vms via wg-flock` |

Services you run beside xdebbied (abe on the underlay, a DNS server, an apt
proxy) need their own rules. Leave `xdebbie` out of their comments, or
bootstrap deletes them as strays.

```bash
sudo ufw status
```

should show, among others:

```text
51820/udp                  ALLOW IN    Anywhere        # xdebbie wireguard underlay
6081/udp on wg-flock       ALLOW IN    Anywhere        # xdebbie geneve on wg-flock
```

## The nftables tables

| Table | Owner | What |
|---|---|---|
| `inet xdebbie_mesh` | the mesh (`[mesh] firewall = true`) | drops Geneve arriving on any interface but the underlay, whatever ufw says |
| `ip xdebbie_host` | bootstrap and `/v1/settings` | masquerades the overlay, sends VM DNS to `upstream_dns`, drops new flows into the bridge from other interfaces (except the underlay with `mesh_routed`), and forwards nothing in or out of the storage bridge |

```bash
sudo nft list table inet xdebbie_mesh
sudo nft list table ip xdebbie_host
```

## Blackhole routes for the mesh networks

While the mesh runs, every box keeps a blackhole route at the worst metric
for each mesh network:

```text
blackhole 10.22.0.0/16 metric 4294967295   # WireGuard underlay
blackhole 10.23.0.0/16 metric 4294967295   # VM overlay
blackhole 10.24.0.0/16 metric 4294967295   # storage network
```

**Why.** These networks are normally reached through connected routes:
`10.22.0.0/16` on `wg-flock`, `10.23.0.0/16` on the bridge. If the link or
its address goes away (the bridge is deleted, the WireGuard link torn down,
the mesh disabled), the connected route goes with it, and without a
blackhole the traffic would follow the default route out to the LAN or the
internet. With it, the packets are dropped: a local sender gets `EINVAL`,
a forwarded packet no ICMP.

**The metric.** Route priority is a 32-bit number and lower wins, so
4294967295 loses to every other route to the same network, and a more
specific route wins by prefix length anyway. The metric is also how the
daemon knows its own: a blackhole at any other metric is never touched.

**Changing them.** `blackholes` in `[mesh]` (or the API spec) lists the
networks. Absent means the three above; `[]` turns the feature off and
removes the ones the daemon installed. Each entry is an IPv4 network,
`a.b.c.d/n` with no host bits set and n from 1 to 32.

**Plain underlay.** Without the managed WireGuard the underlay is yours, so
a listed network that holds any peer's `remote` is skipped: a blackhole
there could beat your route to that peer.

**Teardown keeps them.** Disabling the mesh removes the tunnels and the
WireGuard link but leaves the blackholes, which is the point. To remove
one by hand:

```bash
sudo ip route del blackhole 10.23.0.0/16 metric 4294967295
```

A running mesh puts it back on its next pass unless it is gone from
`blackholes`. `ip route show type blackhole` lists them.

## libvirt over TLS

Each box's libvirtd listens for TLS (16514/tcp) on its overlay bridge
address and accepts client certificates signed by the cluster CA. The
leader reaches a box's libvirt there, across the mesh. Bootstrap copies the
box's cluster certificate into libvirt's places
(`/etc/pki/CA/cacert.pem`, `/etc/libvirt/pki/`, `/etc/pki/libvirt/`) and
sets `listen_tls` in `/etc/libvirt/libvirtd.conf`
([Boxes](../compute/boxes.md#what-bootstrap-does)).

## AppArmor

Debian's `wireguard-tools` confines `/usr/bin/wg` to keys under
`/etc/wireguard`. xdebbied passes the private key to
`wg set ... private-key /dev/stdin` through a pipe, and the package ships
`/etc/apparmor.d/local/wg`, which allows `/var/lib/xdebbie/wg.key` for use
by hand.

## If it goes wrong

- If WireGuard handshakes work but `gnv-bN` RX stays 0 on the far side,
  then that box's ufw lacks the Geneve rule: `sudo xdebbied bootstrap`
  there.
- If you changed `multicast_addr` and the box never sees a peer, then re-run
  bootstrap so the discovery rule follows.
- If a rule you added vanished after bootstrap, then its comment started
  with `xdebbie`.

## Reference

[Ports](../reference/files-and-ports.md#ports),
[`blackholes`](../reference/configuration.md#mesh),
[`/v1/settings`](../reference/api.md#settings).
