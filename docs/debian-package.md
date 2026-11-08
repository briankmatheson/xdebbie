# qfodufk8sd Debian Package

## Overview

The qfodufk8sd Debian package provides a complete, production-ready installation with:

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
cd ~/src/qfodufk8s
./scripts/build-deb.sh
```

This will:
1. Build the release binary
2. Strip debug symbols
3. Create the .deb package
4. Show installation instructions

**Output**: `target/debian/qfodufk8sd_0.1.1_amd64.deb`

## Installation

### Fresh Install

```bash
# Install package
sudo dpkg -i qfodufk8sd_0.1.1_amd64.deb

# Fix dependencies (if needed)
sudo apt-get install -f
```

**What happens on install:**

1. ✅ Dependencies installed (libvirt, qemu, cloud-image-utils)
2. ✅ Directories created:
   - `/etc/qfodufk8s/` - Configuration
   - `/var/lib/qfodufk8s/` - State
3. ✅ Bootstrap runs automatically (creates config, network, pool, token)
4. ✅ Service enabled and started
5. ✅ Environment file created at `~/.qfodufk8s_env`

### Verify Installation

```bash
# Check service status
systemctl status qfodufk8sd

# View logs
journalctl -u qfodufk8sd -f

# Check API
source ~/.qfodufk8s_env
qfodufk8sctl host ls
```

Expected output:
```
● qfodufk8sd.service - qfodufk8s VM Orchestrator
     Loaded: loaded (/lib/systemd/system/qfodufk8sd.service; enabled)
     Active: active (running) since ...
```

## Package Details

### Files Installed

```
/usr/bin/qfodufk8sd                          # Main binary
/lib/systemd/system/qfodufk8sd.service       # Systemd service
/usr/share/doc/qfodufk8sd/README.md          # Documentation
/usr/share/doc/qfodufk8sd/BOOTSTRAP.md       # Bootstrap docs
/etc/qfodufk8s/                              # Config directory
/var/lib/qfodufk8s/                          # State directory
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

**Service file**: `/lib/systemd/system/qfodufk8sd.service`

**Key features:**
- Starts after network-online.target and libvirtd.service
- Runs bootstrap before starting server
- Restarts on failure (5s delay)
- Logs to systemd journal
- Security hardening enabled

**Service control:**

```bash
# Start
sudo systemctl start qfodufk8sd

# Stop
sudo systemctl stop qfodufk8sd

# Restart
sudo systemctl restart qfodufk8sd

# Enable (start on boot)
sudo systemctl enable qfodufk8sd

# Disable (don't start on boot)
sudo systemctl disable qfodufk8sd

# View status
systemctl status qfodufk8sd

# View logs
journalctl -u qfodufk8sd -f
```

## Configuration

### Default Config

Generated automatically at: `/etc/qfodufk8s/qfodufk8sd.toml`

```toml
listen = "0.0.0.0:7443"
state_file = "/var/lib/qfodufk8s/state.json"
token_file = "/etc/qfodufk8s/token"

[[hosts]]
name = "hostname"
uri = "qemu:///system"
bridge = "virbr-qfodufk8s"
storage_pool = "qfodufk8s"
cpu_overcommit = 2.0
mem_overcommit = 1.0
reserved_cpus = 2
reserved_memory_mib = 4096
```

### Customizing Config

```bash
# Edit config
sudo editor /etc/qfodufk8s/qfodufk8sd.toml

# Restart service to apply
sudo systemctl restart qfodufk8sd
```

### Client Configuration

After installation, configure the client:

```bash
# Load environment (creates ~/.qfodufk8s_env on first bootstrap)
source ~/.qfodufk8s_env

# Verify connection
qfodufk8sctl host ls
```

## Upgrading

### Upgrade Package

```bash
# Download new version
wget https://example.com/qfodufk8sd_0.1.2_amd64.deb

# Install (replaces old version)
sudo dpkg -i qfodufk8sd_0.1.2_amd64.deb
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
sudo dpkg -i qfodufk8sd_0.1.1_amd64.deb
```

## Removal

### Remove (Keep Config)

```bash
sudo apt-get remove qfodufk8sd
```

**What happens:**
- ✅ Service stopped and disabled
- ✅ Binary removed
- ⚠️  Config kept at `/etc/qfodufk8s/`
- ⚠️  State kept at `/var/lib/qfodufk8s/`

### Purge (Remove Everything)

```bash
sudo apt-get purge qfodufk8sd
```

**What happens:**
- ✅ Service stopped and disabled
- ✅ Binary removed
- ✅ Config removed (`/etc/qfodufk8s/`)
- ✅ State removed (`/var/lib/qfodufk8s/`)
- ✅ Systemd unit removed

## Troubleshooting

### Service Won't Start

```bash
# Check status
systemctl status qfodufk8sd

# View full logs
journalctl -u qfodufk8sd -n 100

# Check dependencies
systemctl status libvirtd
systemctl status network-online.target
```

Common issues:
- libvirtd not running → `sudo systemctl start libvirtd`
- Network not ready → wait for network-online.target
- Config invalid → check `/etc/qfodufk8s/qfodufk8sd.toml`

### Bootstrap Failed

If bootstrap fails on install:

```bash
# Run manually
sudo qfodufk8sd bootstrap

# Check for errors
sudo qfodufk8sd bootstrap --force

# View config
sudo cat /etc/qfodufk8s/qfodufk8sd.toml
```

### Client Can't Connect

```bash
# Check service is running
systemctl status qfodufk8sd

# Check environment
source ~/.qfodufk8s_env
echo $QFODUFK8S_URL      # Should be http://0.0.0.0:7443
echo $QFODUFK8S_TOKEN    # Should be set

# Test API directly
curl -H "Authorization: Bearer $QFODUFK8S_TOKEN" $QFODUFK8S_URL/hosts
```

### Logs Show Errors

```bash
# View recent logs
journalctl -u qfodufk8sd -n 100

# Follow logs in real-time
journalctl -u qfodufk8sd -f

# Filter by priority
journalctl -u qfodufk8sd -p err

# Export logs
journalctl -u qfodufk8sd > qfodufk8sd.log
```

## Multi-System Deployment

### Deploy to Multiple Hosts

```bash
# Build package once
./scripts/build-deb.sh

# Copy to hosts
for host in node1 node2 node3; do
  scp target/debian/qfodufk8sd_0.1.1_amd64.deb $host:/tmp/
  ssh $host "sudo dpkg -i /tmp/qfodufk8sd_0.1.1_amd64.deb"
done

# Wait for bootstrap and cluster formation
sleep 30

# Check cluster on any node
ssh node1 "source ~/.qfodufk8s_env && qfodufk8sctl host ls"
```

Expected: All three nodes should appear in the list!

### APT Repository (Future)

For easier deployment, consider hosting packages in an APT repository:

```bash
# Add repository
echo "deb https://repo.example.com/debian stable main" | \
  sudo tee /etc/apt/sources.list.d/qfodufk8s.list

# Install
sudo apt-get update
sudo apt-get install qfodufk8sd

# Upgrade
sudo apt-get update
sudo apt-get upgrade qfodufk8sd
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

API token stored at `/etc/qfodufk8s/token` with mode 0600 (root only).

### Network Exposure

Default: Listens on `0.0.0.0:7443` (all interfaces).

For production:
1. Use firewall rules
2. Enable TLS (set tls_cert/tls_key in config)
3. Or bind to localhost only (edit config)

## Integration with Cluster API

Once installed and running, qfodufk8sd can be used with CAPQ:

```bash
# On management cluster, install CAPQ
kubectl apply -f capq-deployment.yaml

# Create workload cluster
kubectl apply -f qfodufk-cluster.yaml

# VMs automatically created via qfodufk8sd!
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
systemctl status qfodufk8sd

# Memory usage
ps aux | grep qfodufk8sd

# Detailed stats
systemd-cgtop -1 | grep qfodufk8sd
```

## Unattended Installation

For automated deployments:

```bash
# Install non-interactively
DEBIAN_FRONTEND=noninteractive sudo dpkg -i qfodufk8sd_0.1.1_amd64.deb
sudo apt-get install -f -y

# Verify
systemctl is-active qfodufk8sd
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
