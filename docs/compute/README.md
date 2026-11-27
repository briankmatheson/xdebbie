# Compute

VMs, where they go, how they move, and the boxes and leader that run them.

One box leads. It keeps the records of hosts, VMs and volumes, runs the
scheduler and the reconciler, and answers every cluster request; the
other boxes forward to it and do host-local work when it asks. Every box
runs the same `xdebbied`.

## Pages

- [VMs](vms.md): create, start, stop, reboot, delete, protect; what a VM
  is made of; phases.
- [Scheduling](scheduling.md): filters, scores, failure domains,
  anti-affinity, host labels, cordon, the reconcile loop.
- [Migration](migration.md): drain a host, move one VM, what live
  migration needs.
- [capx and Cluster API](kubernetes.md): the resources, running capx, what
  the contract asks and how capx meets it.
- [The leader](leader.md): election, forwarding, followers keeping up.
- [Boxes: bootstrap, join, enrol, approve](boxes.md): what bootstrap does,
  enrolment, upgrades, starting over.
- [Security](security.md): the cluster CA, tokens, TLS, who can reach
  what.

## Reference

[`xdctl vm` and `xdctl host`](../reference/xdctl.md#vms),
[`/v1/vms` and `/v1/hosts`](../reference/api.md#vms),
[`[[hosts]]` keys](../reference/configuration.md#hosts).
