# Scheduling

How the leader decides which host a VM goes to, and the loop that makes
the hosts match the records.

## What you need

Two or more hosts make this interesting; with one, every VM goes there if
it fits.

## Ask where a VM would go

```bash
xdctl vm create web2 --image debian-13 --cpus 4 --memory-mib 8192 --dry-run
```

It prints every host that fits, best first, and creates nothing. The API
is `POST /v1/schedule` with a VM spec; it answers the ranking, or 422 with
the reasons. A token in `schedule_token_file` opens this call and nothing
else, for a caller that asks whether one more VM would fit before asking
for it.

## Filters

A host is out if any of these fails. The name in quotes is the reason
`message` shows.

| Filter | Fails when |
|---|---|
| `pinned elsewhere` | the VM is pinned (`--host`) to another host |
| `cordoned` | the host is cordoned |
| `unhealthy` | the host has not been reached for `host_stale_secs` (60) |
| `failure domain` | the VM asks for another failure domain (`--failure-domain`) |
| `not control-plane eligible` | the VM is a control plane and the host has `control_plane_eligible = false` |
| `labels` | the host lacks a label the VM requires (`placement.requiredLabels`; capx `hostSelector`) |
| `capacity unknown` | the host's CPU and memory have not been read yet |
| `cpu`, `memory` | the VM does not fit in what is left |
| `disk` | `local` storage: the root disk is bigger than the pool's free space |
| `no abe`, `abe capacity unknown`, `abe pool full`, `abe capacity` | `abe` storage ([Capacity](../storage/abe.md#capacity)) |
| `strict anti-affinity` | `--strict`, and a VM of the same group is already there |

A VM that fits nowhere stays `Pending`, and `message` counts the reasons,
for example `no host satisfies the request: 2×memory, 1×cordoned`.

## Scores

Hosts that pass are ranked by, in order:

1. Fewest VMs of the same anti-affinity group on the host.
2. Fewest VMs of the same group in the host's failure domain.
3. Use of the fullest resource after placement (CPU, memory, and for
   `abe` VMs the abe pool): lowest first with `policy = "spread"` (the
   default), highest first with `policy = "pack"`.
4. Host name, so the answer is the same every time.

Placement is a pure function (`crates/xdebbie-core/src/scheduler.rs`): the
same inputs give the same ranking. Pending VMs are placed one at a time,
each seeing the one before, so a burst of creates does not pile onto one
host.

## Capacity

What a host can take, separately for CPU and memory:

```text
allocatable = (physical - reserved) × overcommit
```

| Key | Daemon default | Bootstrap writes |
|---|---|---|
| `cpu_overcommit` | 4.0 | 2.0 |
| `mem_overcommit` | 1.0 | 1.0 |
| `reserved_cpus` | 2 | 2 |
| `reserved_memory_mib` | 4096 | 4096 |

A box with 16 threads and 64 GiB, as bootstrap sets it up, takes 28 vCPUs
and 60 GiB of VMs. A VM counts against its host from the moment it is
placed until it is deleted, running or not.

## Failure domains

Each host has a `failure_domain` (default `default`): a rack, a PDU, a
room. `--failure-domain FD` keeps a VM in one. Cluster API spreads control
planes across domains, and capx passes each Machine's domain on as a hard
rule.

```bash
xdctl fd        # NAME CP HOSTS
```

A domain is `CP` (control-plane capable) if any host in it is
`control_plane_eligible`.

## Anti-affinity

`--group G` puts a VM in a group. Soft by default: the scheduler spreads a
group across hosts, then across failure domains, but places it anyway when
it must. `--strict` makes it hard: never two of the group on one host.
capx groups each control plane and each MachineDeployment
(`NAMESPACE.CLUSTER.GROUP`).

## Host labels

Hosts carry labels (`labels` in `[[hosts]]`, or
`xdctl host add ... --label disk=nvme`). A VM spec's
`placement.requiredLabels` (capx: `hostSelector`) keeps it to hosts with
all of them. `xdctl vm create` has no flag for it; use the API or capx.

## Cordon and uncordon

```bash
xdctl host cordon HOST      # no new VMs; running ones stay
xdctl host uncordon HOST
```

`xdctl host ls` shows `Cordoned`. To move the running ones off too, drain
([Migration](migration.md)).

## The reconcile loop

Every `reconcile_interval_secs` (5), on the leader:

1. **Inventory.** Read every host's CPU, memory, pool space and abe reading
   in parallel. Success stamps `lastSeen`; a host not seen for
   `host_stale_secs` (60) is unhealthy and gets no new VMs.
2. **Schedule.** Place pending VMs, one at a time.
3. **Converge.** Move every other VM one step towards what it should be,
   all at once: provision, start, stop, reboot, migrate, delete, attach
   volumes.

The loop never holds the store lock while it talks to a hypervisor: it
works from a snapshot and merges the result, and a delete that arrives
mid-provision wins. Every step can be interrupted and run again.

The records live in one JSON file, `state_file`
(`/var/lib/xdebbie/state.json`), rewritten atomically on every change.
For tens of hosts and hundreds of VMs that is fast and easy to back up.

Hosts registered by another box's daemon carry an `agent` URL; the leader
asks that daemon to do the host-local work (thin-pool disks, NVMe
connects, the seed, libvirt calls) through `POST /v1/agent/OP`. Hosts added
by hand are driven over their libvirt `uri`.

## If it goes wrong

- If a VM stays `Pending` with `cpu` or `memory`, then free room, add a
  host, or raise the overcommit.
- If it says `unhealthy`, then the leader has not reached that host's
  libvirt for 60 seconds: check the host and the mesh.
- If it says `pinned elsewhere`, then the host it is pinned to is not
  registered yet; a box registers itself within seconds of its daemon
  starting.
- If everything lands on one host, then check `policy`: `pack` does that on
  purpose.

## Reference

[`policy`, `reconcile_interval_secs`, `host_stale_secs`](../reference/configuration.md#top-level-keys),
[`[[hosts]]`](../reference/configuration.md#hosts),
[`POST /v1/schedule`](../reference/api.md#vms),
[`xdctl host`](../reference/xdctl.md#hosts).
