# Storage

Where VM disks live, where base images come from, and what to back up.

A VM's disks live in one of two places, chosen per VM with `--storage`
(`spec.storage`; capx: `spec.template.spec.storage`):

| `storage` | Root disk | Data disk (`--data-disk-gib`) | Reachable from |
|---|---|---|---|
| `local` (default) | a thin LVM snapshot of `base-IMAGE`, or a qcow2 overlay on `IMAGE.qcow2` in the host's pool | a thin LV, or a raw volume in the pool | the host that made it |
| `abe` | an NVMe/TCP volume from the host's abe, cloned or filled from the base image | an NVMe/TCP volume, blank | any host that reaches the exporter |

Every VM also has a small cloud-init seed image (`NAME-seed.iso`) in the
host's libvirt pool, whatever its storage.

Separately, **volumes** are abe disks with their own life: you create them,
plug them into a VM, unplug them, grow them. Kubernetes uses them through
abe-csi.

## Pages

- [Local disks](local.md): the libvirt pool, LVM thin pools, base images,
  data disks.
- [abe-backed disks](abe.md): setting up abe on a box, how VM disks are
  made on it, base volumes, capacity, the storage bridge.
- [Volumes](volumes.md): standalone abe volumes, hot-plugged, grown.
- [Backups](backups.md): what is kept where, and how to restore a box.

## What is kept where

| What | Where | Lost with the box? |
|---|---|---|
| Hosts, VMs, volumes (the records) | `/var/lib/xdebbie/state.json` on each box; followers mirror the leader's every 5 seconds | no, while another box mirrors it |
| Mesh spec, settings, client config, enrolments, DNS reservations | `/var/lib/xdebbie/*.json` | yes |
| WireGuard identity | `/var/lib/xdebbie/wg.key` | yes |
| Config, token, certificates, the CA key (first box) | `/etc/xdebbie/` | yes |
| `local` VM disks and seeds | the host's LVM thin pool or `/var/lib/libvirt/xdebbie` | yes |
| `abe` disks and volumes | the abe of the host that made them | with that host's abe |

## Growing disks

VMs are immutable: to give one a bigger root disk, make a new VM and delete
the old one (Cluster API does exactly that on a rolling update). cloud-init
grows the root filesystem to the disk at first boot. Volumes can grow in
place: [`xdctl volume grow`](volumes.md#grow-a-volume).

## Reference

[Storage keys](../reference/configuration.md#top-level-keys)
(`volume_group`, `thin_pool`) and
[host keys](../reference/configuration.md#hosts) (`storage_pool`, `abe`,
`abe_*`), [`xdctl volume` and `xdctl image`](../reference/xdctl.md#volumes),
[`/v1/volumes`](../reference/api.md#volumes).
