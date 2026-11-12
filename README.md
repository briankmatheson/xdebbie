# XDebbie

**Self-forming cloud orchestration** — A lightweight VM scheduler and orchestrator that
automatically discovers peers, elects leaders, and manages VMs across small clusters
of Linux servers.

XDebbie turns 3-30 ordinary servers running QEMU/KVM + libvirt into a self-organizing
cloud. Copy a single `.deb` file, install it, and it automatically:
- Installs dependencies (libvirt, qemu, cloud-image-utils)
- Creates networks and storage pools
- Discovers other XDebbie nodes via multicast
- Elects a leader using the Bully algorithm
- Schedules VMs across the cluster with HA failover

No configuration, no coordination service, no shared storage. Just `dpkg -i` and go.

```
                ┌─────────────────── management cluster ───────────────────┐
                │  CAPI core + kubeadm providers                           │
  kubectl ───▶  │  capx  (XDebbie8sCluster / XDebbie8sMachine controllers) │
                └─────────────────────────────┬────────────────────────────┘
                                              │ HTTPS + bearer token
                                   ┌──────────▼──────────┐
  xdctl ──────────────────▶ │     xdebbied      │  state.json
                                   │ API · scheduler ·   │
                                   │ reconciler          │
                                   └──┬──────┬──────┬────┘
                         libvirt over │ ssh  │      │
                ┌─────────────────────▼┐ ┌───▼────┐ ┌▼────────────────┐
                │ srv1  KVM            │ │ srv2   │ │ srvN            │
                │ pool: LVM thin pool  │ │  ...   │ │  ...            │
                │ br0 ── VMs           │ │        │ │                 │
                └──────────────────────┘ └────────┘ └─────────────────┘
```

## Features

✅ **Self-Bootstrapping** — Install on stock Debian 13, auto-configure everything  
✅ **Auto-Discovery** — Multicast peer discovery on 239.0.0.42:7442  
✅ **Leader Election** — Bully algorithm with score-based ranking  
✅ **High Availability** — Automatic failover in <15s  
✅ **Zero Config** — No coordination service, no manual peer lists  
✅ **LVM Thin Provisioning** — Instant VM disk creation (<1s) via snapshots  
✅ **Placement Engine** — Spread/pack policies with anti-affinity  
✅ **Live Migration** — Move VMs between hosts without shared storage  
✅ **Cluster API** — Full v1beta2 infrastructure provider  
✅ **Encrypted Mesh** — L2 overlay between hosts: Geneve over a WireGuard underlay the daemon manages, on by default with zero peers (`xdctl mesh`)  
✅ **Single Binary** — 9 MB daemon + 6 MB CLI tool  
✅ **Debian Package** — Systemd service with proper dependencies  

## Components

| Crate | Binary | What it does |
|---|---|---|
| `xdebbie-core` | — | Data model and the placement engine (pure, exhaustively tested) |
| `xdebbied` | `xdebbied` | REST API, discovery, election, scheduler, reconciler, libvirt driver |
| `xdctl` | `xdctl` | CLI for hosts, VMs, failure domains, drain, migrate |
| `capx` | `capx` | Cluster API infrastructure provider (contract **v1beta2**) |

## How It Works

### Discovery & Election

XDebbie nodes use **multicast UDP (239.0.0.42:7442)** to automatically discover each other:

1. **ANNOUNCE** (every 5s): Node broadcasts hostname, API URL, capacity
2. **HEARTBEAT** (every 1s): Node sends score and leader status
3. **ELECTION**: Bully algorithm elects highest-scored node as leader
4. **FAILOVER**: New leader elected within 15s if current leader fails

The leader decides: it runs the scheduler and reconciler and answers every
cluster request. Followers forward `/v1` calls to it, and `xdctl` asks
`/v1/leader` first and talks to the leader directly. One box with no peers
(or `enable_discovery = false`) is its own leader.

**Score Calculation** (0-3501 points):
- Uptime (0-1000): Longer-running nodes preferred
- Capacity (0-1000): Larger nodes preferred  
- Load (0-1000): Less-loaded nodes preferred
- Incumbent bonus (+500): Reduces leadership churn
- UUID tiebreaker (0-1): Deterministic when scores equal

See [docs/autodiscovery.md](docs/autodiscovery.md) for complete details.

### Scheduling & Operations

- **Placement**: filter (health, cordon, failure domain, control-plane
  eligibility, labels, CPU/memory/disk with per-host reservations and
  overcommit, strict anti-affinity) then score (anti-affinity spread across
  hosts and failure domains, then `spread` or `pack`). Deterministic.
- **Storage**: LVM thin provisioning with instant snapshots from base images
  (<1s vs 60s+ qcow2 copy), automatic thin pool management, optional data
  disks. Falls back to qcow2 overlays if LVM not configured.
- **Lifecycle**: create (LVM thin snapshot or qcow2 overlay + NoCloud seed),
  start, graceful stop, reboot, delete, self-heal of missing domains, address
  discovery via qemu-guest-agent / ARP / DHCP lease.
- **Operations**: cordon, uncordon, drain (live-migrate everything off a
  host), live migration with incremental storage copy — no shared storage.
- **Cluster API**: everything the v1beta2 InfraCluster / InfraMachine
  contracts require, plus templates for ClusterClass, failure domains,
  pause, `clusterctl move`, MachineHealthCheck remediation, KCP rollouts.
  See [docs/capi-contract.md](docs/capi-contract.md) for the line-by-line mapping.

## Quick Start

### Install (Stock Debian 13)

```bash
# Build the package
./scripts/build-deb.sh

# Copy to each node
scp target/debian/xdebbied_0.7.0-1_amd64.deb node1:/tmp/
scp target/debian/xdebbied_0.7.0-1_amd64.deb node2:/tmp/
scp target/debian/xdebbied_0.7.0-1_amd64.deb node3:/tmp/

# Install on each node
ssh node1 sudo dpkg -i /tmp/xdebbied_0.7.0-1_amd64.deb
ssh node2 sudo dpkg -i /tmp/xdebbied_0.7.0-1_amd64.deb
ssh node3 sudo dpkg -i /tmp/xdebbied_0.7.0-1_amd64.deb
```

**That's it!** Each node will:
1. Auto-install dependencies (libvirt, qemu, etc.)
2. Create network (192.168.100.0/24) and storage pool
3. Generate config and secure token
4. Start the service
5. Discover other nodes via multicast
6. Elect a leader

### Optional: Enable LVM Thin Provisioning

For 96% faster VM disk creation, configure LVM thin provisioning:

```bash
# On each node, create a thin pool
sudo lvcreate -L 200G -T your-vg/xdebbie-pool

# Enable in config (top-level keys, not a table)
sudo tee -a /etc/xdebbie/xdebbied.toml << 'EOF'
volume_group = "your-vg"
thin_pool = "xdebbie-pool"
EOF

# Import a base image as a thin volume
sudo lvcreate -V 25G -T your-vg/xdebbie-pool -n base-debian-13
sudo qemu-img convert -f qcow2 -O raw -t none debian-13-generic-amd64.qcow2 /dev/your-vg/base-debian-13

# Restart service
sudo systemctl restart xdebbied
```

Without LVM configuration, xdebbie falls back to qcow2 overlays (slower but works).

> **Base images must boot headless.** xdebbie domains have no video device.
> If a customised image sets `GRUB_TERMINAL_OUTPUT="gfxterm serial"`, GRUB
> has no display to initialise and silently loops on `Booting 'Debian
> GNU/Linux'` forever. Use `GRUB_TERMINAL="console serial"` and run
> `update-grub` (e.g. via `virt-customize`). Test with
> `qemu-system-x86_64 -vga none -display none -serial stdio ...` -- plain
> `-nographic` still provides a VGA device and will hide the problem.

### Use the CLI

```bash
# Source the environment (on any node)
source /etc/xdebbie/xdebbie_env

# List nodes (should show all 3)
xdctl host ls

# Create a VM
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 4096

# Create a VM with data disk (e.g., for databases)
xdctl vm create db1 --image debian-13 --cpus 4 --memory-mib 8192 --data-disk-gib 100

# List VMs; `ls` is also `list`, `show`, `get`. `describe` prints one object in full.
xdctl vm ls
xdctl vm describe db1
xdctl net ls
```

### Check Discovery

```bash
# View discovery logs
sudo journalctl -u xdebbied -f | grep -i discovery

# Check who's the leader
xdctl leader

# Rotate the API token cluster-wide (old one works until every node has the new)
xdctl token rotate
xdctl token status
```

See [BOOTSTRAP.md](BOOTSTRAP.md) for detailed bootstrap documentation and
[docs/autodiscovery.md](docs/autodiscovery.md) for discovery, election and
what the leader does.

## Documentation

- **[BOOTSTRAP.md](BOOTSTRAP.md)** — Self-bootstrapping on stock Debian 13
- **[docs/autodiscovery.md](docs/autodiscovery.md)** — Discovery, election and the leader
- [Architecture](docs/architecture.md) — State, reconcile loop, scheduler
- [Cluster API contract](docs/capi-contract.md) — What CAPI asks, how CAPX answers
- [REST API](docs/api.md) — HTTP API reference
- [Operations](docs/operations.md) — Drain, upgrades, backup, security, limits
- [Geneve mesh](docs/mesh.md) — Overlay bridge stitching between hosts, `/v1/mesh`, `xdctl mesh`
- [Overlay DHCP](docs/network.md) — `[network]`: addresses allocated at VM creation, DHCP answered from the VM store, `/v1/network`, `xdctl net`
- [Storage](docs/storage.md) — `storage = abe`: VM disks as NVMe/TCP volumes from each host's ABE exporter, attachable from any host

## Status

**v0.7.0** — ABE-backed VM disks (NVMe/TCP volumes from a per-host exporter, recorded on the VM, re-attached after reboots), Geneve overlay mesh with a managed WireGuard underlay, built-in overlay DHCP (addresses bound to VM records), LVM thin provisioning, clean VM naming.

The scheduler and reconciler are covered by unit tests against a fake
hypervisor. LVM thin provisioning has been tested in production for instant
VM disk creation. The CAPI provider (CAPX) implements the full v1beta2 contract.

Known gaps and limitations are listed in [docs/operations.md](docs/operations.md#limits).

## License

Copyright © 2026 Brian Matheson and XDebbie8s contributors.

XDebbie8s is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE). Every source file carries an
`SPDX-License-Identifier: GPL-3.0-or-later` header.

Contributions are accepted under the same license with a
[Developer Certificate of Origin](CONTRIBUTING.md#sign-off) sign-off.
