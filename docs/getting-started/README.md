# Getting started

From bare Debian boxes to VMs, a Kubernetes cluster and a laptop on the
mesh, in four short walks.

xdebbie runs VMs on a few Debian 13 boxes as one cluster. Each box runs
`xdebbied`; you drive them with `xdctl`. Do these in order:

1. [One box to a running VM](first-box.md): install the package, create a
   VM, log in to it. About ten minutes.
2. [Join a second box](second-box.md): enrol it, approve it, connect the
   mesh.
3. [A Kubernetes cluster with capx](kubernetes.md): Cluster API builds
   clusters from xdebbie VMs.
4. [A laptop on the mesh with xdj](laptop.md): reach the VMs from outside
   the cluster.

## What you need

- One or more machines with Debian 13 (trixie), amd64, and hardware
  virtualization (`ls /dev/kvm` shows the device).
- Root on each, and a console in case the firewall surprises you.
- Hostnames that resolve. Fully qualified is best.

## Conventions

- Commands to type are in `bash` blocks. Words in capitals, such as
  `NEW_BOX` or `VM`, are placeholders: replace them.
- Examples use the defaults: the WireGuard underlay is `10.22.0.0/16`, the
  VM overlay `10.23.0.0/16`, and box N owns `10.23.N.0/24`. Hostnames are
  under `example.org` (`box0.example.org`, `box1.example.org`); LAN
  addresses come from `192.0.2.0/24`.
- "Box" means a machine running `xdebbied`. Every word we use has a line in
  the [glossary](../glossary.md).

## Next

When the walks are done, the other parts explain each piece:
[Network](../network/README.md), [Storage](../storage/README.md),
[Compute](../compute/README.md), and the [Reference](../reference/README.md)
for exact names and defaults.
