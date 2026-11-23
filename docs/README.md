# XDebbie documentation

Start with the [README](../README.md) (install, join, use) and
[BOOTSTRAP.md](../BOOTSTRAP.md) (what bootstrap does, the files it writes,
joining a cluster).

## Running it

- [Operations](operations.md): drain, upgrades, backup, security, limits
- [Discovery, election and the leader](autodiscovery.md)
- [Geneve mesh over WireGuard](mesh.md): `xdctl mesh`
- [Overlay addressing and DHCP](network.md): `xdctl net`
- [VM storage](storage.md): local disks or ABE NVMe/TCP volumes
- [Aviary-in-a-Box](aviary-in-a-box.md): XDebbie under Aviary

## Reference

- [REST API](api.md)
- [Architecture](architecture.md): state, reconcile loop, scheduler
- [Cluster API contract](capi-contract.md): what Cluster API asks, how capx answers
- [The Debian package](debian-package.md): build, contents, maintainer scripts

Design records live in [../specs](../specs) and [../milestones](../milestones);
every release is in [CHANGELOG.md](../CHANGELOG.md).
