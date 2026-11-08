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
  kubectl ───▶  │  capq  (XDebbie8sCluster / XDebbie8sMachine controllers) │
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
                │ pool: base images    │ │  ...   │ │  ...            │
                │ br0 ── VMs           │ │        │ │                 │
                └──────────────────────┘ └────────┘ └─────────────────┘
```

## Features

✅ **Self-Bootstrapping** — Install on stock Debian 13, auto-configure everything  
✅ **Auto-Discovery** — Multicast peer discovery on 239.0.0.42:7442  
✅ **Leader Election** — Bully algorithm with score-based ranking  
✅ **High Availability** — Automatic failover in <15s  
✅ **Zero Config** — No coordination service, no manual peer lists  
✅ **Placement Engine** — Spread/pack policies with anti-affinity  
✅ **Live Migration** — Move VMs between hosts without shared storage  
✅ **Cluster API** — Full v1beta2 infrastructure provider  
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

**Score Calculation** (0-3501 points):
- Uptime (0-1000): Longer-running nodes preferred
- Capacity (0-1000): Larger nodes preferred  
- Load (0-1000): Less-loaded nodes preferred
- Incumbent bonus (+500): Reduces leadership churn
- UUID tiebreaker (0-1): Deterministic when scores equal

See [DISCOVERY.md](DISCOVERY.md) for complete details.

### Scheduling & Operations

- **Placement**: filter (health, cordon, failure domain, control-plane
  eligibility, labels, CPU/memory/disk with per-host reservations and
  overcommit, strict anti-affinity) then score (anti-affinity spread across
  hosts and failure domains, then `spread` or `pack`). Deterministic.
- **Lifecycle**: create (thin qcow2 overlay on a host-local base image +
  NoCloud seed), start, graceful stop, reboot, delete, self-heal of missing
  domains, address discovery via qemu-guest-agent / ARP / DHCP lease.
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
scp target/debian/xdebbied_0.3.0-1_amd64.deb node1:/tmp/
scp target/debian/xdebbied_0.3.0-1_amd64.deb node2:/tmp/
scp target/debian/xdebbied_0.3.0-1_amd64.deb node3:/tmp/

# Install on each node
ssh node1 sudo dpkg -i /tmp/xdebbied_0.3.0-1_amd64.deb
ssh node2 sudo dpkg -i /tmp/xdebbied_0.3.0-1_amd64.deb
ssh node3 sudo dpkg -i /tmp/xdebbied_0.3.0-1_amd64.deb
```

**That's it!** Each node will:
1. Auto-install dependencies (libvirt, qemu, etc.)
2. Create network (192.168.100.0/24) and storage pool
3. Generate config and secure token
4. Start the service
5. Discover other nodes via multicast
6. Elect a leader

### Use the CLI

```bash
# Source the environment (on any node)
source /etc/xdebbie/xdebbie_env

# List nodes (should show all 3)
xdctl host ls

# Create a VM
xdctl vm create web1 --image debian-13 --cpus 2 --memory-mib 4096

# List VMs
xdctl vm ls
```

### Check Discovery

```bash
# View discovery logs
sudo journalctl -u xdebbied -f | grep -i discovery

# Check who's the leader
sudo journalctl -u xdebbied | grep "becoming leader"
```

See [BOOTSTRAP.md](BOOTSTRAP.md) for detailed bootstrap documentation and
[DISCOVERY.md](DISCOVERY.md) for discovery/election internals.

## Documentation

- **[BOOTSTRAP.md](BOOTSTRAP.md)** — Self-bootstrapping on stock Debian 13
- **[DISCOVERY.md](DISCOVERY.md)** — Auto-discovery and leader election
- [Architecture](docs/architecture.md) — State, reconcile loop, scheduler
- [Cluster API contract](docs/capi-contract.md) — What CAPI asks, how CAPX answers
- [REST API](docs/api.md) — HTTP API reference
- [Operations](docs/operations.md) — Drain, upgrades, backup, security, limits

## Status

v0.1. The scheduler and reconciler are covered by unit tests against a fake
hypervisor; the libvirt driver and CAPI flow need validation on real hardware
before you trust them with anything you care about. Known gaps are listed in
[docs/operations.md](docs/operations.md#limits).

## License

Copyright © 2026 Brian Matheson and XDebbie8s contributors.

XDebbie8s is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE). Every source file carries an
`SPDX-License-Identifier: GPL-3.0-or-later` header.

Contributions are accepted under the same license with a
[Developer Certificate of Origin](CONTRIBUTING.md#sign-off) sign-off.
