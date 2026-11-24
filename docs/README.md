# XDebbie documentation

XDebbie turns a few Debian 13 servers running QEMU/KVM and libvirt into one
small cloud: it places, runs, moves and heals VMs, joins every box's VM
network into one encrypted mesh, and serves VMs to Kubernetes through
Cluster API.

These pages are organized by what you are doing.

## Getting started

Follow these in order to go from bare servers to a working cluster.

1. [Install the first box](getting-started/install.md)
2. [Join a second box](getting-started/join.md)
3. [Create your first VM](getting-started/first-vm.md)
4. [Run Kubernetes with Cluster API](getting-started/cluster-api.md)

## How-to guides

Steps for one task each.

- [Use an LVM thin pool for VM disks](how-to/lvm-thin-pool.md)
- [Use ABE volumes](how-to/abe-volumes.md)
- [Drain a host and move VMs](how-to/drain-and-migrate.md)
- [Connect a laptop with xdj](how-to/mesh-clients.md)
- [Reserve addresses through DNS](how-to/dns-reserved-addresses.md)
- [Give VMs NTP servers through DHCP](how-to/ntp.md)
- [Rotate the API token](how-to/rotate-token.md)
- [Upgrade xdebbied](how-to/upgrade.md)
- [Back up and restore a box's state](how-to/backup-restore.md)

## Reference

Exact names, defaults and formats.

- [Configuration file: xdebbied.toml](reference/configuration.md)
- [REST API](reference/api.md)
- [xdctl commands](reference/xdctl.md)
- [xdebbied commands](reference/xdebbied.md)
- [Files, ports and interfaces](reference/files-and-ports.md)
- [DHCP options served](reference/dhcp-options.md)
- [Cluster API contract](reference/capi-contract.md)
- [The Debian package](reference/debian-package.md)

## Explanation

How it works and why.

- [Architecture](architecture.md): state, the reconcile loop, the scheduler
- [Bootstrap](bootstrap.md): what bootstrap does, the files it writes,
  joining a cluster
- [Discovery, election and the leader](autodiscovery.md)
- [Geneve mesh over WireGuard](mesh.md)
- [Overlay addressing and DHCP](network.md)
- [VM storage: local or ABE](storage.md)
- [Security model](security.md)

## When things go wrong

- [Troubleshooting](troubleshooting.md)
- [Limits and known gaps](limits.md)

## Conventions

- Commands to type are in `bash` blocks. Words in capitals, such as
  `NEW_BOX` or `VM`, are placeholders: replace them.
- Examples use the product defaults: the WireGuard underlay is
  `10.22.0.0/16` and the VM overlay `10.23.0.0/16`, where box N owns
  `10.23.N.0/24`. Hostnames are under `example.org` (`box0.example.org`,
  `box1.example.org`), and LAN addresses come from `192.0.2.0/24`.

## Building this site

The pages are an [mdBook](https://rust-lang.github.io/mdBook/). From the
top of the source tree:

```bash
mdbook build          # writes the site to book/
mdbook serve --open   # preview with live reload
```

`book/` is a plain static site: publish it with any web server, or with
GitHub or Gitea Pages. Design records live in `specs/` and `milestones/` in
the source tree, and every release is in `CHANGELOG.md`.
