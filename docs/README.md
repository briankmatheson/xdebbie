# xdebbie documentation

xdebbie runs VMs on a few Debian 13 boxes as one cluster: it places them,
moves them, joins every box's VM network into one encrypted mesh, and
serves VMs to Kubernetes through Cluster API. These pages are for xdebbie
0.23.0.

1. [Getting started](getting-started/README.md): one box to a running VM,
   a second box, a Kubernetes cluster, a laptop on the mesh.
2. [Network](network/README.md): the WireGuard underlay, the Geneve overlay,
   addresses and DHCP, clients, routes and firewall, discovery.
3. [Storage](storage/README.md): local disks and base images, abe-backed
   disks, volumes, backups.
4. [Compute](compute/README.md): VMs, scheduling, migration, capx, the
   leader, joining boxes, security.
5. [Reference](reference/README.md): configuration, `xdctl`, `xdebbied`,
   the API, the mesh spec, files and ports, limits, troubleshooting.
6. [Glossary](glossary.md): every noun and verb, with a link to where it
   is explained.

## Building this site

The pages are an [mdBook](https://rust-lang.github.io/mdBook/). From the
top of the source tree:

```bash
mdbook build          # writes the site to book/
mdbook serve --open   # preview with live reload
```

`tests/check-book.py` checks the table of contents and every link without
mdBook. Design records are in `specs/` and `milestones/`, and every release
is in `CHANGELOG.md`.
