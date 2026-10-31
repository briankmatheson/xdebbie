# Architecture

## Design goals

1. **Nothing on the hosts but libvirt.** No agent to upgrade, no daemon to
   secure. A host is a URI.
2. **One source of truth.** qfodufk8sd owns desired state; libvirt reports
   actual state; the reconciler closes the gap.
3. **Idempotent everything.** Every API write can be retried; every
   reconcile step can be interrupted and re-run.
4. **Small enough to read in an afternoon.**

## State

`qfodufk8sd` keeps one JSON document (`state_file`) containing hosts and VMs. It
is rewritten atomically (temp file → `fsync` → `rename`) under the write
lock on every change, so readers never observe state that is not on disk.
For tens of hosts and hundreds of VMs this is fast and trivially backed up.

VM ids are UUIDs chosen by the **client** (`PUT /v1/vms/{id}`). The id is
also the libvirt domain UUID, the cloud-init `instance-id`, and the CAPI
`providerID` (`qfodufk8s://<id>`), so every layer agrees on identity.

## Reconcile loop

Every `reconcile_interval_secs`:

1. **Inventory.** `virsh nodeinfo` and `pool-info` on every host in
   parallel. Success stamps `lastSeen`; a host not seen for
   `host_stale_secs` is unhealthy and receives no new work.
2. **Schedule.** Pending VMs are placed one at a time, each decision seeing
   the previous one, so a burst of creates does not pile onto one host.
3. **Converge.** Every other VM advances one step, concurrently:

```
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

1. `vol-create-as` a qcow2 overlay backed by the base image (thin; only
   writes consume space). cloud-init's growpart expands the root FS.
2. Build a NoCloud seed (`user-data`, `meta-data`, `network-config`) locally
   with `cloud-localds`, then `vol-upload` it to the host pool. No SSH
   besides libvirt's own transport is needed.
3. `define` the domain (q35, host-passthrough CPU, virtio disk/NIC/RNG, guest
   agent channel, serial console), mark autostart, `start`.

MAC addresses are derived from the VM id (`52:54:00:xx:xx:xx`), so DHCP
reservations survive re-creation.

## Scheduler

`qfodufk8s_core::place(spec, hosts, vms, policy, healthy, exclude)` is a pure
function returning a ranked list.

**Filters** (any failure removes the host): pinned host, cordon, health,
failure domain, control-plane eligibility, required labels, CPU, memory,
disk, strict anti-affinity.

Allocatable capacity per host is
`(physical − reserved) × overcommit`, separately for CPU and memory.
Memory overcommit defaults to 1.0; CPU to 4.0.

**Scores**, compared lexicographically:

1. Fewest VMs of the same anti-affinity group on the host.
2. Fewest VMs of the same group in the host's failure domain.
3. Dominant-resource utilization after placement — lowest first for
   `spread`, highest first for `pack`.
4. Host name, so results are reproducible.

`POST /v1/schedule` and `qfodufk8sctl vm create --dry-run` return the whole
ranking without creating anything.

## Live migration

`virsh migrate --live --persistent --undefinesource --copy-storage-inc`
from source to destination. Because base images exist in every pool, only
the overlay and seed travel. qfodufk8sd pre-creates both on the destination,
then deletes them from the source after success.
