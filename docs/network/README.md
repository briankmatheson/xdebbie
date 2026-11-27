# Network

How boxes, VMs and laptops reach each other: a WireGuard underlay between
boxes, one Geneve L2 overlay on top, and xdebbie's own DHCP.

## The picture

```text
            box 0                                   box 1
  VMs 10.23.0.2-254                         VMs 10.23.1.2-254
        |                                           |
  virbr-xdebbie 10.23.0.1  ===== gnv-b1 / gnv-b0 =====  virbr-xdebbie 10.23.1.1
        |              (Geneve, VNI 23, UDP 6081)          |
  wg-flock 10.22.0.1  ======= WireGuard, UDP 51820 =======  wg-flock 10.22.1.1
        |                                           |
       LAN                                         LAN
        |
  wg-flock peer: laptop (xdj) 10.22.3.1, a client of box 0
```

Each box has two mesh interfaces:

- **The underlay**, `wg-flock`: a WireGuard link, box N at `10.22.N.1/16`.
  It is encrypted, and only boxes and clients you added are on it.
- **The overlay bridge**, `virbr-xdebbie`: the libvirt network `xdebbie`,
  box N at `10.23.N.1/16`. Every VM's NIC is a port on it. One Geneve
  tunnel per peer box (`gnv-bN`), riding the underlay, joins the bridges
  into one L2 segment, so VMs on different boxes talk as if they shared a
  switch.

## Addresses

| Network | Default | Box N | Who uses it |
|---|---|---|---|
| Underlay | `10.22.0.0/16` | `10.22.N.1` | WireGuard between boxes and xdj clients; abe's NVMe/TCP |
| Overlay | `10.23.0.0/16` | bridge `10.23.N.1`, VMs `10.23.N.2`-`.254` | VMs |
| Storage (optional) | none; `10.24.0.0/16` in examples | `10.24.N.1` | a second, isolated bridge for abe traffic |

Box ids are 0-253 (assigned at enrolment); client numbers are 1-254 and
share the same space. Each box owns one `/24` of the overlay, so two boxes
never hand out the same address.

## Pages

- [The mesh: underlay, overlay, bridge](mesh.md): peers, FDB modes, MTU,
  conditions.
- [Addresses and DHCP](addresses-and-dhcp.md): how a VM gets its address,
  the options it is told, reserved addresses, NTP, NAT and DNS for VMs.
- [Clients and the hub](clients.md): laptops and cloud servers on the
  mesh, routes via the hub, what clients are told about DNS.
- [Routes and firewall](routes-and-firewall.md): blackhole routes, ufw,
  the nftables tables, AppArmor.
- [Discovery](discovery.md): how boxes on one LAN find each other.

## Reference

[`[mesh]` and `[network]` keys](../reference/configuration.md#mesh),
[the mesh spec and status](../reference/mesh-spec.md),
[ports and interfaces](../reference/files-and-ports.md#ports).
