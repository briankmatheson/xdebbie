# Volumes

Standalone abe disks with a life of their own: create one, plug it into a
VM, unplug it, grow it, delete it.

Guests have no route to abe (it listens on the underlay), so block storage
for things inside a VM, such as Kubernetes PersistentVolumes through
abe-csi, goes through xdebbied: the host does the NVMe/TCP, and the guest
sees a local disk. In our fio runs that beat an initiator inside the guest
(3 times the sequential read rate, a sixth of the p99 write latency).

## What you need

- abe set up on at least one host ([abe-backed disks](abe.md#set-up-abe-on-a-host)).
- A running VM to attach to.

## Create and attach a volume

1. Create a blank volume:

   ```bash
   xdctl volume create pvc-demo --gib 5
   ```

2. Note its id:

   ```bash
   xdctl volume ls
   ```

3. Plug it into a VM (name or id):

   ```bash
   xdctl volume attach VOLUME_ID demo
   ```

4. In the guest, find it:

   ```bash
   ls -l /dev/disk/by-id/scsi-0QEMU_QEMU_HARDDISK_VOLUME_ID
   ```

5. When done, unplug and delete it:

   ```bash
   xdctl volume detach VOLUME_ID
   xdctl volume rm VOLUME_ID
   ```

## What you should see

`xdctl volume ls` shows the volume with `ATTACHED` set to the VM and a
`TARGET` such as `sdb` once the reconciler has plugged it in, within a pass
or two.

Volumes go on the VM's virtio-scsi controller (targets `sdb`, `sdc`, ...,
up to 255). The guest sees the whole id at
`/dev/disk/by-id/scsi-0QEMU_QEMU_HARDDISK_ID`. A VM with no PCI slot left
for the controller gets a virtio-blk disk instead (`vdc`, ...), at
`/dev/disk/by-id/virtio-` plus the first 20 characters of the id.

## How it behaves

- **Create** is synchronous: the caller needs abe's id. It goes to the abe
  with the most headroom unless `--host` names one, and asks abe for
  `name=vol-NAME`, so a retry gets the same volume. Concurrent creates of
  one name make one abe call: the first answers 201, the rest 200 with the
  same record. 507 means no abe has room, and says why for each host; 503
  that no healthy host has an abe.
- **Attach and detach** are reconciled like everything else: `attachTo` is
  the wish, `attached {vm, host, target}` what happened, `message` why not.
  The host the VM runs on does the `nvme connect` and the hot-plug. The
  attach is re-run every pass while wanted, so a host reboot or a
  re-created domain heals itself. A failed attach is retried after 5
  seconds, doubling up to 300.
- **Attach to a running VM that still has it** is refused (409) unless you
  pass `--force`, which moves it.
- **Delete** marks the record `deleting` first, then calls abe. If abe
  fails, the call answers 502, the record stays `deleting`, nothing
  attaches it, and a second `rm` finishes the job. Delete is refused while
  attached.
- **A deleted VM** drops its volumes: they survive, unattached.

## Grow a volume

```bash
xdctl volume grow VOLUME_ID --gib 20
```

It never shrinks; a size no bigger than now changes nothing. It needs abe
0.7.2 or later (501 otherwise), rounds up to a MiB, and answers 409 when
abe's kernel cannot re-read the size live and 507 when abe has no room.
Where the volume is attached, the VM's host passes the new size to the
guest (`virsh blockresize`) within a pass or two. Growing the filesystem
is the guest's job; abe-csi does it for a PVC.

## QoS

abe 0.7.0 and later run an IOPS arbiter. `GET /v1/qos` merges every host's
report, cached for 2 seconds; an abe that is down, older or has the arbiter
off adds nothing. xdebbied only relays these numbers: abe-csi enforces them
in the guest (cgroup `io.max`).

## If it goes wrong

- If `create` answers 507, then no abe has room; the message lists each
  host's reason (`abe capacity`, `abe pool full`, `abe capacity unknown`,
  `no abe`, `unhealthy`).
- If `attach` answers 409 *volume is being deleted*, then a delete failed
  half-way: run `xdctl volume rm VOLUME_ID` again.
- If the volume never shows `ATTACHED`, then `xdctl volume describe ID`
  has the reason in `message`.

## Reference

[`xdctl volume`](../reference/xdctl.md#volumes),
[`/v1/volumes` and `/v1/qos`](../reference/api.md#volumes),
[Limits: storage](../reference/limits.md#storage).
