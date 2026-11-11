# Changelog

All notable changes to XDebbie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0-2] - 2026-11-10

### Fixed
- **Bootstrap created the libvirt network with the subnet's network address
  as its `<ip address>`** (`192.168.100.0` instead of `192.168.100.1`), so the
  host had no usable gateway on the bridge. `ensure_network` now derives the
  first host address and the netmask from the CIDR for any prefix 8–30.
- **Bootstrap continued after `systemctl start libvirtd` failed** and then
  died three steps later with "failed to connect socket". It now probes the
  URI (`virsh uri`); if nothing answers it stops immediately and prints
  `systemctl status libvirtd` so the real cause (e.g. a `243/CREDENTIALS`
  TPM-sealed secret-driver key on Debian testing) is visible.

## [0.4.0-1] - 2026-11-10

### Added
- **Geneve overlay mesh (X12).** New `mesh` module in `xdebbied` stitches the
  overlay bridge of every host into one L2 segment: one `gnv-b<P>` Geneve
  tunnel per peer (same VNI/dstport, different remote) enslaved to the bridge
  with `isolated on`; `derived` mode pins 17 static FDB entries per peer and
  disables learning/flooding on tunnel ports; MTU = underlay − 50 applied to
  tunnels, bridge and new VM NICs; `nft table inet xdebbie_mesh` drops the
  Geneve port on non-underlay interfaces; in-process ICMP probes with
  hysteresis drive per-peer `Up/Down/Error` and `Ready/Degraded` conditions.
  Reconciliation is derived from kernel state (zero writes on a converged
  restart) and only ever touches links named `gnv-b*` carrying the
  `xdebbie-mesh/v1` alias. Configured via a `[mesh]` TOML section (validated
  at startup) or the new `/v1/mesh` API (`GET`/`PUT` with `If-Match`/`DELETE`,
  `/v1/mesh/peers/{box}`, `POST /v1/mesh/reconcile`), persisted in
  `/var/lib/xdebbie/mesh.json`; API state shadows TOML.
- `xdctl mesh ls|get|apply|peer add|peer rm|delete|reconcile`.
- `xdebbied serve --mesh-only` runs only the mesh manager and its routes.
- `tests/mesh/`: 3-host network-namespace harness and M1–M13 acceptance
  tests (run unprivileged under `unshare -Urnm`).
- Package ships `/etc/modules-load.d/xdebbie-mesh.conf` (`geneve`) and now
  depends on `iproute2` and `nftables`. The generated `xdebbied.toml` carries
  a commented-out `[mesh]` section; the mesh is **off by default**.

## [Unreleased] - 2026-11-08

### Added
- **Guest fstrim on every boot via NoCloud vendor-data.** Seeds now carry a
  vendor-data script (run once by cloud-init `scripts-vendor`, so it never
  merges with the caller's user-data) that installs `xdebbie-fstrim.service`
  -- a oneshot `fstrim -av` after `local-fs.target` -- and enables the distro
  `fstrim.timer`. Freed blocks go back to the thin pool instead of pinning
  allocation at the high-water mark. `seed_command` gains a `{vendor}`
  placeholder; configs without it get `--vendor-data` injected automatically
  when the builder is `cloud-localds`.

### Changed
- **LVM root disks honour `diskGib`** with a 25 GiB floor. Previously a thin
  snapshot silently inherited the base image's size (e.g. 10 GiB) regardless
  of what was requested. Now `create_root_disk` runs `lvextend` after the
  snapshot, and grows existing undersized root LVs on reconcile. The guest's
  cloud-init growpart/resizefs picks up the space on next boot.

### Fixed
- **VMs not coming back after host reboot**: LVM thin pool (and every VM disk
  in it) stayed inactive after reboot because `thin-provisioning-tools`
  (`thin_check`) was not installed, so libvirt autostart failed for all VMs.
  - `lvm2` and `thin-provisioning-tools` are now hard dependencies (deb
    `Depends:` and bootstrap `REQUIRED_PACKAGES`)
  - `LvmManager::ensure_activated()` runs `vgchange -ay -K` at startup and
    reports a clear, actionable error if the thin pool is still inactive
  - After storage activation, xdebbied starts any libvirt domains that are
    marked autostart but shut off (`virsh list --autostart --inactive`)
  - `xdebbied.service` now orders after `lvm2-monitor`, `libvirtd`, `virtqemud`
- **Bootstrap libvirtd check**: Changed from fatal error to warning when libvirtd.service fails
  - Allows bootstrap to continue when libvirt uses socket activation
  - Bootstrap now works on systems where libvirtd.service is broken but socket activation works
  - Emits warning: "libvirtd service not available (this is OK if using socket activation)"

## [0.3.2] - 2026-11-08

### Fixed
- **Systemd dependency issue**: Changed `Requires=libvirtd.service` to `Wants=libvirtd.service virtqemud.service`
  - Resolves "Dependency failed" error on systems with modular libvirt daemons
  - Now works with both monolithic libvirtd and modular virtqemud
  - Compatible with Debian 12+, Ubuntu 22.04+ (default modular setup)
  - Better error messages if libvirt is missing entirely

### Changed
- Systemd service: Hard dependency → Soft preference
- Added support for virtqemud (modular QEMU daemon)
- Service now starts even if libvirt daemons not present (fails with clear error later)

## [0.3.1] - 2026-11-08

### Fixed
- **Bootstrap systemd failure**: Environment file now written to `/etc/xdebbie/xdebbie_env` instead of `/root/.xdebbie_env`
  - Resolves "Read-only file system" error when service runs with ProtectHome=true
  - System-wide environment file accessible to all users
  - Graceful error handling (warns instead of failing)
- **Documentation**: Updated all references from `~/.xdebbie_env` to `/etc/xdebbie/xdebbie_env`
- **Installation**: Service now starts successfully after `dpkg -i`

### Changed
- Environment file constant: `.xdebbie_env` → `xdebbie_env`
- Bootstrap now writes to system directory instead of home directory
- postinst script updated with correct environment file location

## [0.3.0] - 2026-11-08

### Added
- **Auto-Discovery**: Multicast peer discovery on 239.0.0.42:7442
  - Automatic peer detection via multicast UDP
  - ANNOUNCE messages every 5s with node capabilities
  - HEARTBEAT messages every 1s for liveness
  - Zero-configuration clustering
  - Enabled by default, can be disabled
- **Leader Election**: Bully algorithm with score-based ranking
  - Score components: uptime (0-1000) + capacity (0-1000) + load (0-1000) + incumbent bonus (+500) + UUID tiebreaker
  - Automatic failover in <15s
  - ELECTION_PROPOSE and ELECTION_VOTE messages
  - Majority voting for leader selection
  - Self-stabilizing algorithm
- **High Availability**:
  - Leader runs active scheduler
  - Followers maintain shadow state
  - Automatic leader election on failure
  - No single point of failure
  - Peer timeout and cleanup (15s)
- **Documentation**:
  - Discovery and election guide (DISCOVERY.md)
  - Network requirements and troubleshooting
  - Monitoring and debugging instructions

### Changed
- Package description updated to reflect discovery features
- Config file includes discovery settings (enable_discovery, multicast_addr)
- Bootstrap generates discovery-enabled config by default
- Binary size increased to ~9.1 MB (includes discovery code)

### Technical
- New modules: `discovery/` (engine, messages, multicast, peer) and `election/` (state, scoring)
- Dependencies: Added `uuid` with serde support, `num_cpus`
- Discovery engine runs as async tokio task
- Multicast socket with TTL=1 (local network only)

## [0.2.0] - 2026-11-08

### Added
- **Self-Bootstrapping**: Complete automatic setup on stock Debian 13
  - Auto-installs dependencies (libvirt, qemu, cloud-image-utils)
  - Auto-creates libvirt network (virbr-xdebbie on 192.168.100.0/24)
  - Auto-creates storage pool (/var/lib/libvirt/xdebbie)
  - Generates configuration file (/etc/xdebbie/xdebbied.toml)
  - Generates secure API token (256-bit)
  - Creates client environment file (/etc/xdebbie/xdebbie_env)
- **Debian Package**: Professional .deb with systemd integration
  - Systemd service with proper network/disk ordering
  - Auto-runs bootstrap on first install
  - Starts on boot, restarts on failure
  - Clean upgrade/removal paths
  - Maintainer scripts (postinst, prerm, postrm)
- **CLI Tool (xdctl)**: Included in Debian package
  - `xdctl host ls` - List hypervisor hosts
  - `xdctl vm ls` - List virtual machines
  - `xdctl vm create` - Create VMs
  - `xdctl vm delete` - Delete VMs
  - JSON output support (`--json`)
- **Documentation**:
  - Bootstrap guide (BOOTSTRAP.md)
  - Quick start guide (docs/quickstart.md)
  - Migration guide (docs/migration.md)
  - Debian package documentation (docs/debian-package.md)
  - Complete design documents (docs/design/)

### Changed
- **Renamed from qfodufk8s to XDebbie**
  - qfodufk8sd → xdebbied (daemon)
  - qfodufk8sctl → xdctl (CLI tool)
  - CAPQ → CAPX (Cluster API Provider)
  - All paths updated (/etc/xdebbie, /var/lib/xdebbie, etc.)
  - Environment variables (XDEBBIE_URL, XDEBBIE_TOKEN)

### Fixed
- Package name typo in description ("XDebbie8s" → "XDebbie")
- OpenSSL development dependencies documented
- CAPX module reference (qfodufk8s.rs → xdebbie.rs)

## [0.1.0] - 2026-11-02

### Added
- Initial release as qfodufk8s
- VM orchestration via libvirt/QEMU
- HTTP API for VM management (:7443)
- Placement policies (spread/binpack)
- State persistence
- Cloud-init support (NoCloud datasource)
- Cluster API Provider (CAPQ)
- Basic CLI tool (qfodufk8sctl)

[0.3.2]: https://github.com/yourusername/xdebbie/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yourusername/xdebbie/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yourusername/xdebbie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yourusername/xdebbie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/yourusername/xdebbie/releases/tag/v0.1.0
