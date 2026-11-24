# Reserve addresses through DNS

Keep VMs off addresses you have named in DNS, such as a load balancer pool
or a Kubernetes API endpoint.

With `reserve_from_dns`, xdebbied reads your zones and never gives a VM an
address that has an A record there. DNS becomes the one list to keep.

## 1. Allow zone transfers to every box

xdebbied reads each zone by zone transfer (AXFR over TCP). On your DNS
server, allow transfers of the zone to each box's address. With BIND, for
example:

```text
zone "mesh.example.org" {
    type primary;
    file "/etc/bind/db.mesh.example.org";
    allow-transfer { 192.0.2.10; 192.0.2.11; };
};
```

## 2. Name the zones

In `[network]` on the first box, add:

```toml
reserve_from_dns = [
  { zone = "mesh.example.org", server = "10.23.0.11" },
]
```

`server` is an IP address, never a name (an `IP:PORT` works too). Boxes
that join later copy the setting with the rest of `[network]`; add it by
hand on boxes that already joined.

Restart xdebbied:

```bash
sudo systemctl restart xdebbied
```

## 3. Check it

```bash
xdctl net ls
```

It lists what DNS reserves and when each zone was last transferred. A
`REFUSED` there means the DNS server does not allow the transfer.

From now on:

- New VMs never get an address that has an A record in those zones.
- `xdctl vm create --ip ADDRESS` on a reserved address is refused, naming
  the record.
- The zones are read again every minute. If a transfer fails, the last
  good set stays in force; it is also saved in `dns-reserved.json` beside
  the state file, so a box that starts while DNS is down keeps its
  reservations.
- A VM keeps an address it already has, even if DNS names it later.

Addresses with no name go in `reserved = ["ADDRESS", ...]` instead.
