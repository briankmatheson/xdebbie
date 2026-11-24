# XDebbie

**A small, self-hosted cloud for ordinary Linux servers.** XDebbie turns a
handful of Debian 13 machines running QEMU/KVM and libvirt into one cluster
that places, runs, moves and heals VMs, and serves them to Kubernetes
through Cluster API.

Install the package on the first box and it becomes a cluster of one: it
sets up libvirt's network and storage, makes the cluster's certificate
authority and API token, and starts serving. Every other box joins with one
command and an operator's approval. Between boxes, XDebbie builds an
encrypted WireGuard mesh with a shared Layer 2 overlay, so VMs on different
hosts sit on one network.

```
              ┌──────────────────── management cluster ────────────────────┐
              │  CAPI core + kubeadm providers                             │
  kubectl ──▶ │  capx (XDebbie8sCluster / XDebbie8sMachine controllers)    │
              └──────────────────────────────┬─────────────────────────────┘
                                             │ HTTPS + bearer token
                                  ┌──────────▼──────────┐
  xdctl ───────────────────────▶  │      xdebbied       │  state.json
                                  │ API · scheduler ·   │
                                  │ reconciler          │
                                  └──┬──────┬──────┬────┘
                        libvirt over │ ssh  │      │
               ┌─────────────────────▼┐ ┌───▼────┐ ┌▼────────────────┐
               │ srv1  KVM            │ │ srv2   │ │ srvN            │
               │ pool: LVM thin pool  │ │  ...   │ │  ...            │
               │ br0 ── VMs           │ │        │ │                 │
               └──────────────────────┘ └────────┘ └─────────────────┘
                 └──── WireGuard underlay + Geneve overlay mesh ────┘
```

## Features

- **One package.** `apt install xdebbied` on stock Debian 13; the first
  start bootstraps the box: libvirt network and storage pool, config, API
  certificate and token.
- **Join by approval.** A new box asks the first one for a certificate;
  an operator approves it with `xdctl approve HOSTNAME`, and it receives
  the cluster token over mutual TLS. Nothing to copy by hand.
- **HTTPS from the first start.** One CA per cluster; every API call is
  over TLS with a bearer token or a cluster-signed client certificate.
- **Encrypted mesh, on by default.** A managed WireGuard underlay
  (`10.22.0.0/16`) carries a Geneve overlay that bridges every box's VM
  network (`10.23.0.0/16`) into one. See `xdctl mesh`.
- **Built-in DHCP.** Each VM gets its address when it is created; the
  responder on the bridge answers only for VMs it knows.
- **Placement.** Spread or pack, failure domains, anti-affinity,
  per-host reservations and overcommit. Deterministic and unit-tested.
- **Fast disks.** LVM thin snapshots of a base image (under a second),
  qcow2 overlays where LVM is not set up, or ABE NVMe/TCP volumes that any
  host can attach.
- **Operations.** Cordon, drain, and live migration without shared
  storage.
- **Cluster API.** `capx` is a full v1beta2 infrastructure provider.
- **Optional discovery and election.** Off by default. Switched on, boxes
  find each other by signed multicast, elect a leader, and fail over if
  it goes away.

## Components

| Crate | Binary | What it does |
|---|---|---|
| `xdebbie-core` | (library) | Data model and the placement engine (pure, exhaustively tested) |
| `xdebbied` | `xdebbied` | API, bootstrap, enrolment, mesh, DHCP, scheduler, reconciler, libvirt driver |
| `xdctl` | `xdctl` | CLI for hosts, VMs, enrolment, the mesh, the overlay network, volumes and images |
| `capx` | `capx` | Cluster API infrastructure provider (contract **v1beta2**) |

The `xdebbied` package ships `xdebbied` and `xdctl`. `capx` is a container
image for the management cluster; `capx run` reads `XDEBBIE_URL`,
`XDEBBIE_TOKEN` and `XDEBBIE_CA` (the cluster CA, as a PEM path or the PEM
text), the same three variables `/etc/xdebbie/xdebbie_env` exports for
`xdctl`.

## Quick start

### 1. Add the apt repository (every box)

```bash
sudo curl -fsSLo /usr/share/keyrings/xdebbie-archive-keyring.gpg \
     https://apt.xdebbie.org/xdebbie-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/xdebbie-archive-keyring.gpg] https://apt.xdebbie.org stable main" \
  | sudo tee /etc/apt/sources.list.d/xdebbie.list
sudo apt update
```

### 2. The first box

```bash
sudo apt install xdebbied
```

The package bootstraps the box and starts the service. Bootstrap first
listens 20 seconds for a running cluster; hearing none, it starts a new
one and this box keeps the cluster CA. It prints the CA's fingerprint:
note it for the boxes that join (`xdctl ca` shows it again).

### 3. Every other box

```bash
sudo apt install xdebbied
sudo xdebbied bootstrap --join https://FIRST_BOX:23443 --ca-fingerprint sha256:FINGERPRINT
```

The new box waits for approval. On any box already in the cluster:

```bash
xdctl enroll ls          # who is asking, with each key's fingerprint
xdctl approve NEW_BOX    # sign its certificate and assign its mesh box id
```

The new box picks up its certificate, the cluster token and its share of
the overlay network, and finishes on its own. If the package's own
bootstrap on this box hears the cluster first, it stops and tells you to
re-run with `--join`: it never starts a second cluster by accident.

### 4. Use the CLI

The env file holds the API URL, token and CA. Root and the `xdebbie`
group can read it:

```bash
sudo usermod -aG xdebbie "$USER"     # then log in again
source /etc/xdebbie/xdebbie_env

xdctl host ls
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 4096
xdctl vm create db1 --image debian-13 --cpus 4 --memory-mib 8192 --data-disk-gib 100
xdctl vm ls                          # also: list, show, get
xdctl vm describe db1
xdctl net ls
xdctl mesh ls
xdctl version
```

`xdctl token rotate` replaces the API token on every box; the old one
keeps working until every box has the new one.

### Optional: LVM thin provisioning

```bash
sudo lvcreate -L 200G -T VG/xdebbie-pool

# top-level keys in /etc/xdebbie/xdebbied.toml, not a table
sudo tee -a /etc/xdebbie/xdebbied.toml <<'EOF'
volume_group = "VG"
thin_pool = "xdebbie-pool"
EOF

# a base image as a thin volume
sudo lvcreate -V 25G -T VG/xdebbie-pool -n base-debian-13
sudo qemu-img convert -f qcow2 -O raw -t none debian-13-generic-amd64.qcow2 /dev/VG/base-debian-13

sudo systemctl restart xdebbied
```

Without it, VM disks are qcow2 overlays: slower to create, otherwise the
same.

> **Base images must boot headless.** XDebbie's VMs have no video device.
> An image with `GRUB_TERMINAL_OUTPUT="gfxterm serial"` loops forever on
> `Booting 'Debian GNU/Linux'`. Use `GRUB_TERMINAL="console serial"` and
> run `update-grub` (for example with `virt-customize`). Test with
> `qemu-system-x86_64 -vga none -display none -serial stdio ...`; plain
> `-nographic` still adds a VGA device and hides the problem.

## How it works

**Bootstrap** (`xdebbied bootstrap`, run by the package and before every
start) installs anything missing, makes sure libvirtd runs, creates the
`xdebbie` network (bridge `virbr-xdebbie` at `10.23.BOX.1`) and storage
pool, and writes `/etc/xdebbie/xdebbied.toml`, the token, the API
certificate and the env file. It is safe to run again; `--force`
regenerates the config and keeps your settings. See
[BOOTSTRAP.md](BOOTSTRAP.md).

**Who decides.** One box leads: it runs the scheduler and reconciler and
answers every cluster request; the others forward to it, and `xdctl`
talks to it directly (`xdctl leader`). With discovery off (the default),
a box with no peers is its own leader. With discovery on
(`enable_discovery = true`, or `PUT /v1/settings` at runtime), boxes
announce themselves on `239.0.0.42:23442`, sign every message with the
cluster token, elect the best-scored box, and elect again within about
15 seconds if the leader goes away.

**Scheduling.** Filter (health, cordon, failure domain, control-plane
eligibility, labels, CPU, memory and disk after reservations and
overcommit, strict anti-affinity), then score (spread across hosts and
failure domains, then `spread` or `pack`).

**VM lifecycle.** Create from an LVM thin snapshot, qcow2 overlay or ABE
volume plus a NoCloud seed; start, graceful stop, reboot, delete; re-create
a domain that went missing; find addresses through the guest agent, ARP or
the DHCP lease.

## Files

| Path | What |
|---|---|
| `/etc/xdebbie/xdebbied.toml` | config, every setting commented (root:xdebbie 0440) |
| `/etc/xdebbie/xdebbie_env` | `XDEBBIE_URL`, `XDEBBIE_TOKEN`, `XDEBBIE_CA` for `xdctl` (root:xdebbie 0440) |
| `/etc/xdebbie/token` | API bearer token (root 0400) |
| `/etc/xdebbie/api-*.pem` | cluster CA, this box's certificate and key |
| `/var/lib/xdebbie/` | state, the WireGuard key, enrolment records |
| `journalctl -u xdebbied` | the daemon's log |

## Documentation

Installed with the package, in `/usr/share/doc/xdebbied/`:

- **README.md**: this file
- **BOOTSTRAP.md**: bootstrap, the files it writes, joining a cluster
- **mesh.md**: the WireGuard underlay and Geneve overlay, `xdctl mesh`
- **network.md**: overlay addressing and the built-in DHCP, `xdctl net`

In the source tree, under `docs/`: `autodiscovery.md` (discovery,
election and the leader), `architecture.md`, `capi-contract.md` (what
Cluster API asks and how capx answers), `api.md` (the REST API),
`operations.md` (drain, upgrades, backup, security, limits) and
`storage.md` (ABE volumes).

## Status

**0.21.0.** The scheduler and reconciler are unit-tested against a fake
hypervisor; the mesh, enrolment and Cluster API provider run the author's
own clusters. Known gaps are listed in
[docs/operations.md](docs/operations.md#limits); releases are in
[CHANGELOG.md](CHANGELOG.md).

## License

Copyright © 2026 Brian Matheson and XDebbie contributors.

XDebbie is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option)
any later version. See [LICENSE](LICENSE). Every source file carries an
`SPDX-License-Identifier: GPL-3.0-or-later` header.

Contributions are accepted under the same license with a
[Developer Certificate of Origin](CONTRIBUTING.md#sign-off) sign-off.
