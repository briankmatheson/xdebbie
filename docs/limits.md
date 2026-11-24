# Limits and known gaps

What XDebbie does not do yet, so you can plan around it.

## Cluster and control plane

- **Discovery is off by default.** Each box then leads alone, with its own
  store. Requests sent to one box are decided there. Point every client at
  the same box, or turn discovery on so the boxes elect one leader.
- **Discovery needs one L2 segment with multicast.** Boxes on different
  networks cannot elect a leader together.
- **Stores are per box.** With discovery on, followers mirror the leader's
  records every 5 seconds, so a new leader starts from a copy a few seconds
  old. There is no replicated store.
- **Joining a box the package already bootstrapped needs a manual reset.**
  With discovery off, the package makes a new box a cluster of its own, and
  `--join` keeps that identity. See
  [Join a second box](getting-started/join.md#2-clear-the-boxs-own-cluster).
- **Token rotation reaches only boxes known through discovery.** With
  discovery off, copy the new token to the other boxes by hand.
- **The CA lives on one box.** Without the first box (or a backup of its CA
  key), no box can join and no certificate can be signed.

## VMs

- **x86_64 only.** The domain template is q35 with `host-passthrough`.
- **VMs are immutable.** To resize, create a new VM and delete the old one.
- **No graceful-stop timeout.** `stop` sends an ACPI shutdown; a guest that
  ignores it stays up. Delete always uses a hard `destroy`.
- **No dynamic DHCP pool.** The responder answers only VMs xdebbied
  created.
- **No DNS for VM names.** VMs' DNS is forwarded to `upstream_dns`; their
  names are not registered anywhere.

## Storage and migration

- **Host-local disks stay on their host.** A dead host's `local` VMs cannot
  be restarted elsewhere. Cluster API's MachineHealthCheck replaces them
  instead.
- **Live migration needs** the same base image, pool name and bridge name
  on both hosts, compatible CPUs, name resolution between hosts, and TCP
  49152-49215 open between them. Bootstrap's firewall does not open that
  range.
- **No live migration of `abe` VMs.** Refused.
- **No live migration of LVM-backed VMs between boxes.**
- **No garbage collection of ABE base volumes.** Delete a sealed base on
  ABE by hand to pick up a changed image.

## Mesh

- **`fdb = "derived"` does not cover VM MACs.** VM MACs come from the VM's
  UUID, not the slot MACs the static FDB installs. If VMs on different boxes
  cannot reach each other, use `fdb = "learn"`.
- **Peers are added by hand on each box.** Enrolment exchanges certificates
  but not WireGuard peers; run `xdctl mesh peer add` both ways.
- **Box ids 0-253, clients 1-254**, so at most 254 boxes per overlay.

## Cluster API

- **No MachinePool, no IPAM contract, no in-place updates.** See
  [Cluster API contract](reference/capi-contract.md#not-yet-implemented).
- **No load balancer for the API endpoint.** Use kube-vip.
- **capx runs as a single replica.** It has no leader election.
- **Deployment files are out of date.** `Dockerfile` and
  `deploy/capf.yaml` still use the provider's pre-release name. Use
  [the steps in Getting started](getting-started/cluster-api.md) instead.

## Observability

- **No metrics endpoint.** The mesh keeps a Prometheus-style snapshot in
  `GET /v1/mesh` (`status.metrics`) until there is one.
