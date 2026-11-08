# Changelog

All notable changes to XDebbie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.3.1]: https://github.com/yourusername/xdebbie/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yourusername/xdebbie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yourusername/xdebbie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/yourusername/xdebbie/releases/tag/v0.1.0
