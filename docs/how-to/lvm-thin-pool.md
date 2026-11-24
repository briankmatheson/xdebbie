# Use an LVM thin pool for VM disks

Make VM root disks thin snapshots of a base image, so a new VM's disk is
ready in under a second.

Without this, VM disks are qcow2 overlays in the `xdebbie` storage pool.
They work the same, but take longer to create. Do this on every host.

You need a volume group with free space. The examples call it `VG`.

## 1. Create the thin pool

```bash
sudo lvcreate -L 200G -T VG/xdebbie-pool
```

## 2. Tell xdebbied

`volume_group` and `thin_pool` are top-level keys. Put them near the top of
`/etc/xdebbie/xdebbied.toml`, before the first `[table]`:

```toml
volume_group = "VG"
thin_pool = "xdebbie-pool"
```

`bootstrap --force` keeps both.

## 3. Import a base image

`xdctl image import` copies an image into the pool as the thin LV
`base-NAME` and trims its free space:

```bash
sudo xdctl image import --format qcow2 --name debian-13 debian-13-generic-amd64.qcow2
```

It reads the volume group and pool from the config. The image must
[boot headless](../troubleshooting.md#a-vm-never-finishes-booting).
`virt-sparsify` comes from the `guestfs-tools` package; without it, add
`--no-sparsify`.

## 4. Restart and check

```bash
sudo systemctl restart xdebbied
xdctl vm create thin-test --image debian-13
xdctl vm ls
```

The VM's root disk is a thin snapshot of `base-debian-13`:

```bash
sudo lvs VG
```

## Updating an image

Import the new version under the same name with `--force`. Disks already
cloned from the old base are not affected.

```bash
sudo xdctl image import --force --format qcow2 --name debian-13 debian-13-generic-amd64.qcow2
```
