# Drain a host and move VMs

Take a host out of service for maintenance, and move single VMs between
hosts.

## Cordon: stop new VMs landing on a host

```bash
xdctl host cordon HOST
xdctl host ls              # STATUS shows Cordoned
```

Running VMs stay where they are. To accept new VMs again:

```bash
xdctl host uncordon HOST
```

## Drain: move everything off

```bash
xdctl host drain HOST
```

Drain cordons the host, then plans a live migration for every running VM on
it. It prints the plan: `moves` (VM to new host) and `stuck` (VMs it cannot
move, with the reason, such as a VM that is not running). Watch the VMs
leave:

```bash
xdctl vm ls
```

When no VM is left on the host, do your maintenance, then
`xdctl host uncordon HOST`.

## Move one VM

```bash
xdctl vm migrate VM --to HOST
```

Leave out `--to` to let the scheduler pick the best host.

## What live migration needs

xdebbied runs `virsh migrate --live --persistent --undefinesource
--copy-storage-inc`: the VM's overlay disk and seed are copied while it
runs, and the base image is expected on both hosts. That means:

- the same base image on both hosts;
- the same storage pool name and bridge name on both hosts;
- CPUs compatible with each other (VMs use `host-passthrough`);
- the hosts can resolve each other's names, and reach each other on
  libvirt's migration ports, TCP 49152-49215. Bootstrap's firewall does
  not open these; add a rule on the underlay if you migrate between boxes.

It does not work for:

- VMs with `storage = abe`: refused.
- VMs on LVM thin disks moving between boxes: the destination's disks are
  made from the source box, so they are not.

See [Limits and known gaps](../limits.md).

## Kubernetes nodes

For VMs that are Cluster API machines, prefer `kubectl drain` on the node
and let Cluster API replace the Machine, instead of migrating it. That
exercises the path you rely on when a host fails.

## Remove a host for good

Drain it, then:

```bash
xdctl host rm HOST
```

`rm` is refused while VMs remain on the host.
