# xdebbied Debian Package

## Overview

The xdebbied Debian package provides a complete, production-ready installation with:

- ✅ Automatic dependency installation
- ✅ Systemd service integration
- ✅ Automatic bootstrap on first install
- ✅ Starts after network and libvirt are ready
- ✅ Automatic restart on failure
- ✅ Clean upgrade and removal

## Building the Package

### Prerequisites

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cargo-deb
cargo install cargo-deb
```

### Build

```bash
cd ~/src/xdebbie
./scripts/build-deb.sh
```

This will:
1. Build the release binary
2. Strip debug symbols
3. Create the .deb package
4. Show installation instructions

**Output**: `target/debian/xdebbied_0.1.1_amd64.deb`

## Installation

### Fresh Install

```bash
# Install package
sudo dpkg -i xdebbied_0.1.1_amd64.deb

# Fix dependencies (if needed)
sudo apt-get install -f
```

**What happens on install:**

1. ✅ Dependencies installed (libvirt, qemu, cloud-image-utils)
2. ✅ Directories created:
   - `/etc/xdebbie/` - Configuration
   - `/var/lib/xdebbie/` - State
3. ✅ Bootstrap runs automatically (creates config, network, pool, token)
4. ✅ Service enabled and started
5. ✅ Environment file created at `~/.xdebbie_env`

### Verify Installation

```bash
# Check service status
systemctl status xdebbied

# View logs
journalctl -u xdebbied -f

# Check API
source ~/.xdebbie_env
xdctl host ls
```

Expected output:
```
● xdebbied.service - xdebbie VM Orchestrator
     Loaded: loaded (/lib/systemd/system/xdebbied.service; enabled)
     Active: active (running) since ...
```

## Package Details

### Files Installed

```
/usr/bin/xdebbied                          # Main binary
/lib/systemd/system/xdebbied.service       # Systemd service
/usr/share/doc/xdebbied/README.md          # Documentation
/usr/share/doc/xdebbied/BOOTSTRAP.md       # Bootstrap docs
/etc/xdebbie/                              # Config directory
/var/lib/xdebbie/                          # State directory
```

### Dependencies

**Required** (automatically installed):
- libvirt-daemon-system
- libvirt-clients
- qemu-kvm
- qemu-utils
- cloud-image-utils
- systemd

### Systemd Service

**Service file**: `/lib/systemd/system/xdebbied.service`

**Key features:**
- Starts after network-online.target and libvirtd.service
- Runs bootstrap before starting server
- Restarts on failure (5s delay)
- Logs to systemd journal
- Security hardening enabled

**Service control:**

```bash
# Start
sudo systemctl start xdebbied

# Stop
sudo systemctl stop xdebbied

# Restart
sudo systemctl restart xdebbied

# Enable (start on boot)
sudo systemctl enable xdebbied

# Disable (don't start on boot)
sudo systemctl disable xdebbied

# View status
systemctl status xdebbied

# View logs
journalctl -u xdebbied -f
```

## Configuration

### Default Config

Generated automatically at: `/etc/xdebbie/xdebbied.toml`

```toml
listen = "0.0.0.0:7443"
state_file = "/var/lib/xdebbie/state.json"
token_file = "/etc/xdebbie/token"

[[hosts]]
name = "hostname"
uri = "qemu:///system"
bridge = "virbr-xdebbie"
storage_pool = "xdebbie"
cpu_overcommit = 2.0
mem_overcommit = 1.0
reserved_cpus = 2
reserved_memory_mib = 4096
```

### Customizing Config

```bash
# Edit config
sudo editor /etc/xdebbie/xdebbied.toml

# Restart service to apply
sudo systemctl restart xdebbied
```

### Client Configuration

After installation, configure the client:

```bash
# Load environment (creates ~/.xdebbie_env on first bootstrap)
source ~/.xdebbie_env

# Verify connection
xdctl host ls
```

## Upgrading

### Upgrade Package

```bash
# Download new version
wget https://example.com/xdebbied_0.1.2_amd64.deb

# Install (replaces old version)
sudo dpkg -i xdebbied_0.1.2_amd64.deb
```

**What happens on upgrade:**

1. ✅ Service stopped
2. ✅ Binary replaced
3. ✅ Config preserved (not overwritten)
4. ✅ State preserved
5. ✅ Service restarted

### Rolling Back

```bash
# Reinstall old version
sudo dpkg -i xdebbied_0.1.1_amd64.deb
```

## Removal

### Remove (Keep Config)

```bash
sudo apt-get remove xdebbied
```

**What happens:**
- ✅ Service stopped and disabled
- ✅ Binary removed
- ⚠️  Config kept at `/etc/xdebbie/`
- ⚠️  State kept at `/var/lib/xdebbie/`

### Purge (Remove Everything)

```bash
sudo apt-get purge xdebbied
```

**What happens:**
- ✅ Service stopped and disabled
- ✅ Binary removed
- ✅ Config removed (`/etc/xdebbie/`)
- ✅ State removed (`/var/lib/xdebbie/`)
- ✅ Systemd unit removed

## Troubleshooting

### Service Won't Start

```bash
# Check status
systemctl status xdebbied

# View full logs
journalctl -u xdebbied -n 100

# Check dependencies
systemctl status libvirtd
systemctl status network-online.target
```

Common issues:
- libvirtd not running → `sudo systemctl start libvirtd`
- Network not ready → wait for network-online.target
- Config invalid → check `/etc/xdebbie/xdebbied.toml`

### Bootstrap Failed

If bootstrap fails on install:

```bash
# Run manually
sudo xdebbied bootstrap

# Check for errors
sudo xdebbied bootstrap --force

# View config
sudo cat /etc/xdebbie/xdebbied.toml
```

### Client Can't Connect

```bash
# Check service is running
systemctl status xdebbied

# Check environment
source ~/.xdebbie_env
echo $XDEBBIE_URL      # Should be http://0.0.0.0:7443
echo $XDEBBIE_TOKEN    # Should be set

# Test API directly
curl -H "Authorization: Bearer $XDEBBIE_TOKEN" $XDEBBIE_URL/hosts
```

### Logs Show Errors

```bash
# View recent logs
journalctl -u xdebbied -n 100

# Follow logs in real-time
journalctl -u xdebbied -f

# Filter by priority
journalctl -u xdebbied -p err

# Export logs
journalctl -u xdebbied > xdebbied.log
```

## Multi-System Deployment

### Deploy to Multiple Hosts

```bash
# Build package once
./scripts/build-deb.sh

# Copy to hosts
for host in node1 node2 node3; do
  scp target/debian/xdebbied_0.1.1_amd64.deb $host:/tmp/
  ssh $host "sudo dpkg -i /tmp/xdebbied_0.1.1_amd64.deb"
done

# Wait for bootstrap and cluster formation
sleep 30

# Check cluster on any node
ssh node1 "source ~/.xdebbie_env && xdctl host ls"
```

Expected: All three nodes should appear in the list!

### APT Repository (Future)

For easier deployment, consider hosting packages in an APT repository:

```bash
# Add repository
echo "deb https://repo.example.com/debian stable main" | \
  sudo tee /etc/apt/sources.list.d/xdebbie.list

# Install
sudo apt-get update
sudo apt-get install xdebbied

# Upgrade
sudo apt-get update
sudo apt-get upgrade xdebbied
```

## Security Considerations

### Service Runs as Root

The service runs as root to access libvirt. This is required for:
- Creating/managing VMs
- Creating networks and bridges
- Managing storage pools

Security measures in place:
- NoNewPrivileges=true
- PrivateTmp=true
- ProtectSystem=strict
- ProtectHome=true
- Limited ReadWritePaths

### Token Security

API token stored at `/etc/xdebbie/token` with mode 0600 (root only).

### Network Exposure

Default: Listens on `0.0.0.0:7443` (all interfaces).

For production:
1. Use firewall rules
2. Enable TLS (set tls_cert/tls_key in config)
3. Or bind to localhost only (edit config)

## Integration with Cluster API

Once installed and running, xdebbied can be used with CAPX:

```bash
# On management cluster, install CAPX
kubectl apply -f capq-deployment.yaml

# Create workload cluster
kubectl apply -f qfodufk-cluster.yaml

# VMs automatically created via xdebbied!
```

## Performance

### Resource Usage

Typical resource usage:
- Memory: ~20-50 MB
- CPU: <1% idle, <5% under load
- Disk: ~12 MB binary + config/state

### Monitoring

```bash
# Check resource usage
systemctl status xdebbied

# Memory usage
ps aux | grep xdebbied

# Detailed stats
systemd-cgtop -1 | grep xdebbied
```

## Unattended Installation

For automated deployments:

```bash
# Install non-interactively
DEBIAN_FRONTEND=noninteractive sudo dpkg -i xdebbied_0.1.1_amd64.deb
sudo apt-get install -f -y

# Verify
systemctl is-active xdebbied
```

## Summary

The Debian package provides:

✅ **Zero-touch installation** - dpkg install and go
✅ **Automatic bootstrap** - Config, network, pool, token all created
✅ **Systemd integration** - Starts on boot, restarts on failure
✅ **Clean upgrades** - Preserves config and state
✅ **Easy removal** - Clean or purge options
✅ **Production-ready** - Security hardening, logging, monitoring

**Next**: Deploy to your infrastructure and enjoy self-managing VM clusters!
