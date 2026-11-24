# Files, ports and interfaces

What xdebbied puts on a box, and what it listens on.

## Files

| Path | Owner and mode | What |
|---|---|---|
| `/etc/xdebbie/` | root:xdebbie 0750 | Configuration directory. |
| `/etc/xdebbie/xdebbied.toml` | root:xdebbie 0440 | The [configuration](configuration.md). |
| `/etc/xdebbie/xdebbie_env` | root:xdebbie 0440 | `XDEBBIE_URL`, `XDEBBIE_TOKEN` and `XDEBBIE_CA` for `xdctl`. Source it. |
| `/etc/xdebbie/token` | root:root 0400 | The cluster's API bearer token: 64 hex characters. |
| `/etc/xdebbie/token.previous` | root:root 0400 | The old token, only while a rotation is in progress. |
| `/etc/xdebbie/api-ca.pem` | root:xdebbie 0440 | The cluster CA certificate. |
| `/etc/xdebbie/api-ca-key.pem` | root:root 0400 | The CA's private key. First box only. |
| `/etc/xdebbie/api-cert.pem` | root:xdebbie 0440 | This box's certificate, signed by the cluster CA. |
| `/etc/xdebbie/api-key.pem` | root:root 0400 | This box's private key. Never leaves the box. |
| `/etc/xdebbie/peers/*.pem` | root:xdebbie 0440 | Extra CAs to trust. Usually empty. |
| `/var/lib/xdebbie/state.json` | root | Hosts, VMs and volumes. Rewritten atomically. |
| `/var/lib/xdebbie/mesh.json` | root | The mesh spec set through the API. |
| `/var/lib/xdebbie/settings.json` | root | Runtime switches from `PUT /v1/settings`. |
| `/var/lib/xdebbie/client-config.json` | root | What xdj clients are told, from `PUT /v1/client-config`. |
| `/var/lib/xdebbie/dns-reserved.json` | root | Last good set of DNS-reserved addresses. |
| `/var/lib/xdebbie/enroll.json` | root | Enrolment requests (CA box). |
| `/var/lib/xdebbie/join.json` | root | A pending `bootstrap --join`. |
| `/var/lib/xdebbie/wg.key` | root 0600 | This box's WireGuard private key. Kept when the mesh is torn down. |
| `/var/lib/libvirt/xdebbie/` | root 0711 | The `xdebbie` storage pool: VM disks and seed images, when LVM is not used. |
| `/etc/sysctl.d/90-xdebbie-forward.conf` | root | Turns on IPv4 forwarding. |
| `/etc/modules-load.d/xdebbie-mesh.conf` | root | Loads `geneve` and `wireguard` at boot. |
| `/etc/apparmor.d/local/wg` | root | Lets `wg` read `/var/lib/xdebbie/wg.key`. |
| `/usr/lib/systemd/system/xdebbied.service` | root | The service. |

The `/var/lib/xdebbie/*.json` files sit beside `state_file`; change
`state_file` and they move with it.

## Ports

Bootstrap opens these in ufw (default deny inbound). Each rule carries a
comment starting `xdebbie`.

| Port | Protocol | Accepted from | What |
|---|---|---|---|
| 22 | TCP | anywhere | SSH, so you are never locked out. |
| 23443 | TCP | every network the box has an address on | The xdebbied API (`listen`). |
| 23442 | UDP | local networks | Discovery multicast to 239.0.0.42 (`multicast_addr`). Only while discovery is on. |
| 51820 | UDP | anywhere | The WireGuard underlay (`[mesh.wireguard] listen_port`). |
| 6081 | UDP | the underlay (`wg-flock`) only | Geneve (`[mesh] dstport`). |
| 16514 | TCP | the underlay and the bridge | libvirt TLS. The leader reaches each box's libvirtd here. |
| 67 | UDP | the bridge only | The DHCP responder. |
| 23000-23999 | TCP | the storage bridge only | ABE NVMe/TCP exports, when `storage_cidr` is set. |
| 443 (or the `abe` URL's port) | TCP | the storage bridge only | ABE's API, when `storage_cidr` is set. |
| 49152-49215 | TCP | other hosts | libvirt's live-migration data. Not opened by bootstrap. |

ABE's own ports on the underlay are opened when ABE is set up; see
[Use ABE volumes](../how-to/abe-volumes.md).

## Interfaces and addresses

| Name | What |
|---|---|
| `virbr-xdebbie` | The overlay bridge (libvirt network `xdebbie`). Box N is `10.23.N.1/16`, MAC `52:54:00:23:NN:fe`. |
| `wg-flock` | The WireGuard underlay. Box N is `10.22.N.1/16`. |
| `gnv-bN` | Geneve tunnel to box N, a port of the overlay bridge. |
| `virbr-abe` | Optional storage bridge (libvirt network `abe`). Box N is `10.24.N.1`. |

| Network | Default | What |
|---|---|---|
| Underlay | `10.22.0.0/16` | WireGuard between boxes and xdj clients. |
| Overlay | `10.23.0.0/16` | VMs. Box N owns `10.23.N.0/24`: bridge `.1`, VMs `.2`-`.254`. |
| Storage | none (`10.24.0.0/16` in examples) | Optional storage bridge. |

## nftables tables

| Table | What |
|---|---|
| `inet xdebbie_mesh` | Drops Geneve arriving on any interface but the underlay. |
| `ip xdebbie_host` | Masquerades this box's VM /24, forwards VM DNS to `upstream_dns`, and drops new flows into the bridge from other interfaces. |
