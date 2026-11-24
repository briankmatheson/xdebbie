# Architecture

How xdebbied keeps state, decides where VMs go, and makes libvirt match.

## Design goals

1. **One package per host.** A host runs libvirt, QEMU and xdebbied, and
   nothing else from XDebbie. Every box's daemon is both a member of the
   cluster and the leader's hands on that box: thin-pool disks, NVMe
   connects and seed images are host-local, so the leader asks the daemon
   there (`/v1/agent/*`).
2. **One source of truth.** xdebbied owns desired state; libvirt reports
   actual state; the reconciler closes the gap.
3. **Idempotent everything.** Every API write can be retried; every
   reconcile step can be interrupted and run again.
4. **Small enough to read in an afternoon.**

## Who decides

One box leads. It runs the scheduler and the reconciler and answers every
cluster request; the other boxes forward to it. With discovery off (the
default) every box leads alone. With discovery on, boxes elect a leader and
elect again if it goes away. See
[Discovery, election and the leader](autodiscovery.md).

## State

xdebbied keeps one JSON document (`state_file`) with hosts, VMs and
volumes. It is rewritten atomically (temporary file, `fsync`, `rename`)
under the write lock on every change, so readers never see state that is
not on disk. For tens of hosts and hundreds of VMs this is fast and easy to
back up.

VM ids are UUIDs chosen by the **client** (`PUT /v1/vms/{id}`). The id is
also the libvirt domain UUID, the cloud-init `instance-id`, and the Cluster
API `providerID` (`xdebbie://ID`), so every layer agrees on identity. The
VM's MAC address is derived from it too (`52:54:00:` plus its first three
bytes), and so is its DHCP binding.

## Reconcile loop

Every `reconcile_interval_secs` (5 by default), on the leader:

1. **Inventory.** Read every host's CPU, memory and pool capacity in
   parallel. Success stamps `lastSeen`; a host not seen for
   `host_stale_secs` (60) is unhealthy and gets no new work.
2. **Schedule.** Pending VMs are placed one at a time, each decision seeing
   the previous one, so a burst of creates does not pile onto one host.
3. **Converge.** Every other VM advances one step, concurrently:

```text
Pending ──place──▶ Provisioning ──disk+seed+define+start──▶ Running ◀──▶ Stopped
                         ▲                                     │  ▲
                         └──────── domain missing ─────────────┘  │
                                                    Migrating ────┘
         any ──DELETE──▶ Deleting ──destroy+undefine+vol-delete──▶ (gone)
```

The reconciler never holds the store lock across hypervisor I/O. It
computes a patch from a snapshot and merges it; a delete that arrives while
a VM is mid-provision wins.

## Provisioning a VM

1. **Root disk.** One of:
   - a thin LVM snapshot of `base-IMAGE`, when `volume_group` and
     `thin_pool` are set ([how-to](how-to/lvm-thin-pool.md));
   - a qcow2 overlay backed by `IMAGE.qcow2` in the host's storage pool;
   - an ABE volume, for `storage = abe` ([VM storage](storage.md)).

   cloud-init's growpart grows the root filesystem to the disk.
2. **Seed.** A NoCloud seed (`user-data`, `meta-data`, `network-config`,
   `vendor-data`) built with `seed_command` (`cloud-localds`) and uploaded
   to the host's pool.
3. **Domain.** Define it (q35, `host-passthrough` CPU, virtio disk, NIC and
   RNG, guest agent channel, serial console, no video device), mark it
   autostart, start it.

Addresses are found through the guest agent, ARP or the DHCP binding.

## Scheduler

Placement is a pure function in `xdebbie-core` (`scheduler.rs`): the same
inputs always give the same ranked list of hosts.

**Filters** (any failure removes the host): pinned host, cordon, health,
failure domain, control-plane eligibility, required labels, CPU, memory,
disk (or, for `storage = abe`, the host's ABE capacity), strict
anti-affinity.

Allocatable capacity per host is `(physical − reserved) × overcommit`,
separately for CPU and memory. The daemon's defaults are CPU overcommit 4.0
and memory 1.0; bootstrap writes 2.0 for CPU.

**Scores**, compared in order:

1. Fewest VMs of the same anti-affinity group on the host.
2. Fewest VMs of the same group in the host's failure domain.
3. Dominant-resource utilization after placement: lowest first for
   `spread`, highest first for `pack`.
4. Host name, so results are reproducible.

`POST /v1/schedule` and `xdctl vm create --dry-run` return the whole
ranking without creating anything.

## Live migration

`virsh migrate --live --persistent --undefinesource --copy-storage-inc`
from source to destination. Base images exist in every pool, so only the
overlay, the data disk and the seed travel. xdebbied creates them on the
destination first, and deletes them from the source after success. See
[Drain a host and move VMs](how-to/drain-and-migrate.md) for what it needs.
