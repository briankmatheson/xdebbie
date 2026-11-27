# Local disks

VM disks on the host itself: qcow2 files in the libvirt pool by default,
or thin LVM snapshots, ready in under a second, once you set up a thin
pool.

## What you need

- A box ([One box to a running VM](../getting-started/first-box.md)).
- For LVM: a volume group with free space (`VG` below), on every host.
- For `xdctl image import`: `virt-sparsify` from `guestfs-tools`, or pass
  `--no-sparsify`.

## How local disks are made

Without LVM, everything lives in the libvirt storage pool `xdebbie`, a
directory at `/var/lib/libvirt/xdebbie` (mode 0711) that bootstrap makes:

| Volume | What |
|---|---|
| `IMAGE.qcow2` | a base image, which you put there |
| `VM.qcow2` | the VM's root disk: a qcow2 overlay backed by `IMAGE.qcow2` |
| `VM-data.raw` | the data disk, with `--data-disk-gib` |
| `VM-seed.iso` | the cloud-init seed |

With `volume_group` and `thin_pool` set, disks are logical volumes in that
group:

| LV | What |
|---|---|
| `base-IMAGE` | a base image, made by `xdctl image import` |
| `VM-root` | the root disk: a thin snapshot of `base-IMAGE`, extended to `--disk-gib` |
| `VM-data` | the data disk, a thin LV |

The seed stays in the pool. A root disk is never smaller than its base. For
images with `k8s` in the name, LVM root disks are at least 25 GiB, since
container images fill small disks fast. cloud-init's growpart grows the
root filesystem to the disk at first boot.

The `local` disks of a VM live only on its host. If the host dies, its
`local` VMs cannot start elsewhere; for Kubernetes nodes, Cluster API's
MachineHealthCheck replaces them.

## Use an LVM thin pool

Do this on every host.

1. Create the thin pool:

   ```bash
   sudo lvcreate -L 200G -T VG/xdebbie-pool
   ```

2. Tell xdebbied. `volume_group` and `thin_pool` are top-level keys: put
   them near the top of `/etc/xdebbie/xdebbied.toml`, before the first
   `[table]`:

   ```toml
   volume_group = "VG"
   thin_pool = "xdebbie-pool"
   ```

   Set both. `bootstrap --force` keeps them.

3. Import a base image (next section), then restart:

   ```bash
   sudo systemctl restart xdebbied
   ```

4. Make a VM and look at its disk:

   ```bash
   xdctl vm create thin-test --image debian-13
   sudo lvs VG
   ```

`sudo lvs VG` lists `thin-test-root`, a snapshot of `base-debian-13`.

## Base images

A base image must exist on every host a VM may land on, under the same
name. Debian's cloud images work.

The image must boot without a screen you can see. VMs have a serial
console and a virtio video device with VNC on the host's `127.0.0.1`; an
image whose GRUB writes only to the screen looks hung on the console
([A VM never finishes booting](../reference/troubleshooting.md#a-vm-never-finishes-booting)).

**Without LVM**, copy the image into the pool under the name you will use,
and tell libvirt:

```bash
sudo cp debian-13-generic-amd64.qcow2 /var/lib/libvirt/xdebbie/debian-13.qcow2
sudo virsh pool-refresh xdebbie
```

**With LVM**, import it. `xdctl image import` runs on the host as root and
never talks to xdebbied:

```bash
sudo xdctl image import --format qcow2 --name debian-13 debian-13-generic-amd64.qcow2
```

It creates `base-debian-13` at the image's virtual size, copies only the
written parts (`qemu-img convert --target-is-zero -S 4k`), then trims the
free space of the filesystems inside (`virt-sparsify --in-place`), so the
base takes no more of the pool than its files. The source file is not
changed.

| Option | Default | Meaning |
|---|---|---|
| `--name NAME` | the file name without its extension | the name `--image` uses |
| `--format raw\|qcow2` | `raw` | the file's format; never guessed |
| `--vg VG`, `--pool POOL` | `volume_group`, `thin_pool` from the config | where to put it |
| `--config PATH` | `/etc/xdebbie/xdebbied.toml` | the config to read them from |
| `--force` | off | replace an existing base |
| `--no-sparsify` | off | skip the trim |

To update an image, import the new version under the same name with
`--force`. Disks already made from the old base keep working, and so does
an abe base already sealed from that name
([Base volumes](abe.md#base-volumes)).

## Data disks

`--data-disk-gib N` adds a second, blank disk: a raw volume in the pool, a
thin LV with LVM, or an abe volume with `--storage abe`. It lives and dies
with the VM. For a disk that outlives its VM, use a
[volume](volumes.md).

## If it goes wrong

- If provisioning fails with *Base image 'VG/base-NAME' not found*, then
  import it on that host: every host needs its own copy.
- If a VM's disk is bigger than you asked, then the image has `k8s` in its
  name (25 GiB minimum) or the base is bigger than `--disk-gib`.
- If thin pool disks stay inactive after a reboot, then
  `thin-provisioning-tools` is missing (the package depends on it, so
  something removed it).

## Reference

[`volume_group`, `thin_pool`](../reference/configuration.md#top-level-keys),
[`storage_pool`](../reference/configuration.md#hosts),
[`xdctl image import`](../reference/xdctl.md#images).
