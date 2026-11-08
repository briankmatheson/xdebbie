# qfodufk8sd Quick Start Guide

## Single-Command Setup

### Fresh Debian 13 System

```bash
# Copy binary to system
scp qfodufk8sd debian13:/usr/local/bin/

# Bootstrap and start (one command!)
ssh debian13 "sudo qfodufk8sd serve --bootstrap"
```

That's it! The system will:
- Install libvirt, QEMU, and dependencies
- Create network and storage pool
- Generate config and API token
- Start the server

## Two-Step Setup (Recommended)

```bash
# Step 1: Bootstrap
sudo qfodufk8sd bootstrap

# Step 2: Start server
sudo qfodufk8sd serve
```

## Client Configuration

```bash
# Load environment (token and URL)
source ~/.qfodufk8s_env

# Test connectivity
qfodufk8sctl host ls
```

## Common Commands

### Bootstrap Operations

```bash
# Standard bootstrap
sudo qfodufk8sd bootstrap

# Force re-bootstrap (overwrite config/token)
sudo qfodufk8sd bootstrap --force

# Custom network
sudo qfodufk8sd bootstrap --network internal --subnet 10.100.0.0/16

# Custom pool
sudo qfodufk8sd bootstrap --pool mypool --pool-path /mnt/vms
```

### Server Operations

```bash
# Start server (uses existing config)
sudo qfodufk8sd serve

# Start with bootstrap
sudo qfodufk8sd serve --bootstrap

# Custom config file
sudo qfodufk8sd serve --config /tmp/custom.toml

# Check hosts connectivity
sudo qfodufk8sd check
```

### Client Operations

```bash
# List hosts
qfodufk8sctl host ls

# List VMs
qfodufk8sctl vm ls

# Create VM
qfodufk8sctl vm create --name test --cpus 2 --memory 2048 --disk 20 --image debian-13-k8s-v1.36

# Delete VM
qfodufk8sctl vm delete test
```

## Files and Locations

```
/etc/qfodufk8s/
├── qfodufk8sd.toml    # Configuration
└── token              # API bearer token (0600)

/var/lib/qfodufk8s/
└── state.json         # Runtime state

/var/lib/libvirt/qfodufk8s/  # Default storage pool

~/.qfodufk8s_env       # Client environment (source this)
```

## Default Network

```
Network:  qfodufk8s
Bridge:   virbr-qfodufk8s
Subnet:   192.168.100.0/24
DHCP:     192.168.100.100 - 192.168.100.250
```

## Verify Setup

```bash
# Check libvirt resources
sudo virsh net-list
sudo virsh pool-list

# Check configuration
cat /etc/qfodufk8s/qfodufk8sd.toml

# Check token
ls -la /etc/qfodufk8s/token

# Test API
source ~/.qfodufk8s_env
curl -H "Authorization: Bearer $QFODUFK8S_TOKEN" $QFODUFK8S_URL/hosts
```

## Troubleshooting

### Bootstrap fails: "not running as root"
```bash
# Run with sudo
sudo qfodufk8sd bootstrap
```

### Server fails: "error loading config"
```bash
# Check config exists
ls -la /etc/qfodufk8s/qfodufk8sd.toml

# Re-bootstrap if needed
sudo qfodufk8sd bootstrap --force
```

### Client fails: "401 Unauthorized"
```bash
# Load environment
source ~/.qfodufk8s_env

# Verify variables set
echo $QFODUFK8S_URL
echo $QFODUFK8S_TOKEN
```

### libvirtd not running
```bash
# Start manually
sudo systemctl start libvirtd
sudo systemctl enable libvirtd

# Or re-bootstrap
sudo qfodufk8sd bootstrap
```

## CAPQ Integration

### Setup for Cluster API

```bash
# 1. Bootstrap qfodufk8sd
sudo qfodufk8sd bootstrap
sudo qfodufk8sd serve &

# 2. Configure client
source ~/.qfodufk8s_env

# 3. Verify
qfodufk8sctl host ls

# 4. In CAPI management cluster, create QfodufkCluster:
kubectl apply -f - <<EOF
apiVersion: infrastructure.cluster.x-k8s.io/v1alpha1
kind: QfodufkCluster
metadata:
  name: workload1
  namespace: default
spec:
  controlPlaneEndpoint:
    host: 192.168.100.10
    port: 6443
EOF
```

## Dependencies Installed

When bootstrapping on stock Debian 13:

- libvirt-daemon-system
- libvirt-clients (virsh)
- qemu-kvm
- qemu-utils (qemu-img)
- cloud-image-utils (cloud-localds)

## Environment Variables

```bash
# Set by ~/.qfodufk8s_env
QFODUFK8S_URL=http://0.0.0.0:7443
QFODUFK8S_TOKEN='<64-char-hex-token>'

# Optional overrides
QFODUFK8SD_CONFIG=/path/to/config.toml  # Default: /etc/qfodufk8s/qfodufk8sd.toml
```

## Systemd Service (Manual)

Create `/etc/systemd/system/qfodufk8sd.service`:

```ini
[Unit]
Description=Qfodufk8s VM Orchestrator
After=libvirtd.service
Requires=libvirtd.service

[Service]
Type=simple
ExecStart=/usr/local/bin/qfodufk8sd serve
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
sudo systemctl daemon-reload
sudo systemctl enable qfodufk8sd
sudo systemctl start qfodufk8sd
```

## Next Steps

1. **Build the binary**: `cd src/qfodufk8s && cargo build --release`
2. **Test bootstrap**: `sudo target/release/qfodufk8sd bootstrap`
3. **Start server**: `sudo target/release/qfodufk8sd serve`
4. **Test client**: `source ~/.qfodufk8s_env && qfodufk8sctl host ls`
5. **Deploy CAPQ**: See CAPQ documentation for Cluster API integration

## More Information

- Full documentation: [BOOTSTRAP.md](src/qfodufk8s/BOOTSTRAP.md)
- Implementation details: [BOOTSTRAP_IMPLEMENTATION_SUMMARY.md](BOOTSTRAP_IMPLEMENTATION_SUMMARY.md)
- Original design: [QFODUFK8SD_BOOTSTRAP_DESIGN.md](QFODUFK8SD_BOOTSTRAP_DESIGN.md)
