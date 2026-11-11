# xdebbie Documentation

## Quick Start

- [Quickstart Guide](quickstart.md) - Get up and running quickly
- [Bootstrap Guide](../BOOTSTRAP.md) - Self-bootstrapping functionality

## Features

- [Bootstrap](bootstrap-implementation.md) - Automatic setup and configuration
- [Autodiscovery](autodiscovery.md) - Multicast discovery and leader election
- [Geneve mesh](mesh.md) - Optional L2 overlay between hosts
- [Storage](storage.md) - Local disks vs ABE NVMe/TCP volumes (`storage = abe`)
- [Migration Guide](migration.md) - Upgrading from older versions

## Design Documents

- [Bootstrap Design](design/bootstrap.md) - Bootstrap implementation design
- [Autodiscovery Design](design/autodiscovery.md) - Cluster formation design
- [Bootstrap Flow](diagrams/bootstrap-flow.md) - Visual flow diagrams

## Development

- [Autodiscovery Roadmap](autodiscovery-roadmap.md) - Implementation phases

## Scripts

See [scripts/](../scripts/) directory for:
- `build.sh` - Build xdebbied
- `run-bootstrap.sh` - Run bootstrap helper
- `test-bootstrap.sh` - Test bootstrap functionality
- `start.sh` - Start server helper
