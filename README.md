# Qfodufk8s

A lightweight VM scheduler, orchestrator, and lifecycle manager for small
clouds of Linux boxes — and a Cluster API infrastructure provider on top of it.

Qfodufk8s turns three to thirty ordinary servers running QEMU/KVM + libvirt into a
pool that places, boots, migrates, and deletes VMs on demand. It talks to every
host with plain `virsh -c qemu+ssh://…`, so a host needs nothing Qfodufk8s-specific:
no agent, no shared storage, no special network.

```
                ┌─────────────────── management cluster ───────────────────┐
                │  CAPI core + kubeadm providers                           │
  kubectl ───▶  │  capq  (Qfodufk8sCluster / Qfodufk8sMachine controllers) │
                └─────────────────────────────┬────────────────────────────┘
                                              │ HTTPS + bearer token
                                   ┌──────────▼──────────┐
  qfodufk8sctl ──────────────────▶ │     qfodufk8sd      │  state.json
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

## Components

| Crate | Binary | What it does |
|---|---|---|
| `qfodufk8s-core` | — | Data model and the placement engine (pure, exhaustively tested) |
| `qfodufk8sd` | `qfodufk8sd` | REST API, durable state, reconciler, libvirt-over-SSH driver |
| `qfodufk8sctl` | `qfodufk8sctl` | CLI for hosts, VMs, failure domains, drain, migrate |
| `capq` | `capq` | Cluster API infrastructure provider (contract **v1beta2**) |

## What it does

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

## Quick start

```sh
cargo build --release
# on each server
sudo examples/four-node/prep-host.sh --nic eno1 --pubkey "$(cat qfodufk8sd.pub)"
# on the qfodufk8sd box
qfodufk8sd --config /etc/qfodufk8s/qfodufk8sd.toml check     # can we reach every host?
qfodufk8sd --config /etc/qfodufk8s/qfodufk8sd.toml serve
qfodufk8sctl host ls
qfodufk8sctl vm create web1 --image debian-13-k8s-v1.36 --cpus 2 --memory-mib 4096 --dry-run
```

The full walkthrough — four physical servers, a 3+4 node Kubernetes cluster
through Cluster API — is in [examples/four-node](examples/four-node/README.md).

## Documentation

- [Architecture](docs/architecture.md) — state, reconcile loop, scheduler
- [Cluster API contract](docs/capi-contract.md) — what CAPI asks, how capq answers
- [REST API](docs/api.md)
- [Operations](docs/operations.md) — drain, upgrades, backup, security, limits

## Status

v0.1. The scheduler and reconciler are covered by unit tests against a fake
hypervisor; the libvirt driver and CAPI flow need validation on real hardware
before you trust them with anything you care about. Known gaps are listed in
[docs/operations.md](docs/operations.md#limits).

## License

Copyright © 2026 Brian Matheson and Qfodufk8s contributors.

Qfodufk8s is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE). Every source file carries an
`SPDX-License-Identifier: GPL-3.0-or-later` header.

Contributions are accepted under the same license with a
[Developer Certificate of Origin](CONTRIBUTING.md#sign-off) sign-off.
