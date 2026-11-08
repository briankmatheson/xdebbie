# Changelog

All notable changes to XDebbie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-08

### Added
- **Self-Bootstrapping**: Complete automatic setup on stock Debian 13
  - Auto-installs dependencies (libvirt, qemu, cloud-image-utils)
  - Auto-creates libvirt network (virbr-xdebbie on 192.168.100.0/24)
  - Auto-creates storage pool (/var/lib/libvirt/xdebbie)
  - Generates configuration file (/etc/xdebbie/xdebbied.toml)
  - Generates secure API token (256-bit)
  - Creates client environment file (~/.xdebbie_env)
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

[0.2.0]: https://github.com/yourusername/xdebbie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/yourusername/xdebbie/releases/tag/v0.1.0
