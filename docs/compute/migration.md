# Migration

Take a host out of service, and move running VMs between hosts while they
run.

## What you need

For a live migration between two hosts:

- the same base image on both;
- the same storage pool name and bridge name on both;
- CPUs compatible with each other (VMs use `host-passthrough`);
- each host resolves the other's name and reaches it on libvirt's
  migration ports, TCP 49152-49215. Bootstrap's firewall does not open
  these: add a rule on the underlay if you migrate between boxes, for
  example
  `sudo ufw allow in on wg-flock proto tcp from 10.22.0.0/16 to any port 49152:49215 comment 'libvirt migration'`.

It does not work for:

- VMs with `--storage abe`: refused;
- VMs on LVM disks, between boxes: the destination's disks are made from
  the source box, so they end up on the wrong one and the migration fails.

## Drain a host

1. Drain it. It cordons the host, then plans a live migration for every
   running VM on it:

   ```bash
   xdctl host drain HOST
   ```

2. Watch the VMs leave:

   ```bash
   xdctl vm ls
   ```

3. Do your maintenance, then let VMs back on:

   ```bash
   xdctl host uncordon HOST
   ```

## Move one VM

```bash
xdctl vm migrate web1 --to box1.example.org
```

Leave out `--to` to let the scheduler pick. Only running VMs migrate (409
otherwise).

## Remove a host for good

Drain it, then:

```bash
xdctl host rm HOST
```

`rm` is refused (409) while any VM is still on the host.

## What you should see

`drain` prints its plan: `moves` (VM to new host) and `stuck` (VMs it
cannot move, with the reason, such as a VM that is not running). Moving VMs
show `Migrating`, then `Running` on the new host. A migration that fails
leaves the VM `Running` where it was, with `migration failed: ...` in
`message`.

## How it works

xdebbied runs
`virsh migrate --live --persistent --undefinesource --copy-storage-inc`
from source to destination. Base images exist on every host, so only the
VM's overlay disk, data disk and seed travel: xdebbied creates them on the
destination first, and deletes them from the source after success.

## Kubernetes nodes

For VMs that are Cluster API Machines, prefer `kubectl drain` on the node
and let Cluster API replace the Machine. That is the path you rely on when
a host dies, so it is worth using when it has not.

## If it goes wrong

- If a VM is `stuck` with `phase Stopped`, then it is not running: start
  it and drain again, or delete it.
- If the migration fails with a connection error, then TCP 49152-49215 is
  closed between the hosts, or they cannot resolve each other.
- If it says *live migration of abe-backed VMs is not supported yet*, then
  it is not.

## Reference

[`xdctl host drain`, `xdctl vm migrate`](../reference/xdctl.md#hosts),
[`/v1/hosts/{name}/drain`, `/v1/vms/{uuid}/migrate`](../reference/api.md#hosts),
[Limits](../reference/limits.md).
