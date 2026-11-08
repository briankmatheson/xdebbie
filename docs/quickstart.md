# xdebbied Quick Start Guide

## Single-Command Setup

### Fresh Debian 13 System

```bash
# Copy binary to system
scp xdebbied debian13:/usr/local/bin/

# Bootstrap and start (one command!)
ssh debian13 "sudo xdebbied serve --bootstrap"
```

That's it! The system will:
- Install libvirt, QEMU, and dependencies
- Create network and storage pool
- Generate config and API token
- Start the server

## Two-Step Setup (Recommended)

```bash
# Step 1: Bootstrap
sudo xdebbied bootstrap

# Step 2: Start server
sudo xdebbied serve
```

## Client Configuration

```bash
# Load environment (token and URL)
source ~/.xdebbie_env

# Test connectivity
xdctl host ls
```

## Common Commands

### Bootstrap Operations

```bash
# Standard bootstrap
sudo xdebbied bootstrap

# Force re-bootstrap (overwrite config/token)
sudo xdebbied bootstrap --force

# Custom network
sudo xdebbied bootstrap --network internal --subnet 10.100.0.0/16

# Custom pool
sudo xdebbied bootstrap --pool mypool --pool-path /mnt/vms
```

### Server Operations

```bash
# Start server (uses existing config)
sudo xdebbied serve

# Start with bootstrap
sudo xdebbied serve --bootstrap

# Custom config file
sudo xdebbied serve --config /tmp/custom.toml

# Check hosts connectivity
sudo xdebbied check
```

### Client Operations

```bash
# List hosts
xdctl host ls

# List VMs
xdctl vm ls

# Create VM
xdctl vm create --name test --cpus 2 --memory 2048 --disk 20 --image debian-13-k8s-v1.36

# Delete VM
xdctl vm delete test
```

## Files and Locations

```
/etc/xdebbie/
├── xdebbied.toml    # Configuration
└── token              # API bearer token (0600)

/var/lib/xdebbie/
└── state.json         # Runtime state

/var/lib/libvirt/xdebbie/  # Default storage pool

~/.xdebbie_env       # Client environment (source this)
```

## Default Network

```
Network:  xdebbie
Bridge:   virbr-xdebbie
Subnet:   192.168.100.0/24
DHCP:     192.168.100.100 - 192.168.100.250
```

## Verify Setup

```bash
# Check libvirt resources
sudo virsh net-list
sudo virsh pool-list

# Check configuration
cat /etc/xdebbie/xdebbied.toml

# Check token
ls -la /etc/xdebbie/token

# Test API
source ~/.xdebbie_env
curl -H "Authorization: Bearer $XDEBBIE_TOKEN" $XDEBBIE_URL/hosts
```

## Troubleshooting

### Bootstrap fails: "not running as root"
```bash
# Run with sudo
sudo xdebbied bootstrap
```

### Server fails: "error loading config"
```bash
# Check config exists
ls -la /etc/xdebbie/xdebbied.toml

# Re-bootstrap if needed
sudo xdebbied bootstrap --force
```

### Client fails: "401 Unauthorized"
```bash
# Load environment
source ~/.xdebbie_env

# Verify variables set
echo $XDEBBIE_URL
echo $XDEBBIE_TOKEN
```

### libvirtd not running
```bash
# Start manually
sudo systemctl start libvirtd
sudo systemctl enable libvirtd

# Or re-bootstrap
sudo xdebbied bootstrap
```

## CAPX Integration

### Setup for Cluster API

```bash
# 1. Bootstrap xdebbied
sudo xdebbied bootstrap
sudo xdebbied serve &

# 2. Configure client
source ~/.xdebbie_env

# 3. Verify
xdctl host ls

# 4. In CAPI management cluster, create XDebbieCluster:
kubectl apply -f - <<EOF
apiVersion: infrastructure.cluster.x-k8s.io/v1alpha1
kind: XDebbieCluster
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
# Set by ~/.xdebbie_env
XDEBBIE_URL=http://0.0.0.0:7443
XDEBBIE_TOKEN='<64-char-hex-token>'

# Optional overrides
XDEBBIED_CONFIG=/path/to/config.toml  # Default: /etc/xdebbie/xdebbied.toml
```

## Systemd Service (Manual)

Create `/etc/systemd/system/xdebbied.service`:

```ini
[Unit]
Description=XDebbie8s VM Orchestrator
After=libvirtd.service
Requires=libvirtd.service

[Service]
Type=simple
ExecStart=/usr/local/bin/xdebbied serve
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
sudo systemctl daemon-reload
sudo systemctl enable xdebbied
sudo systemctl start xdebbied
```

## Next Steps

1. **Build the binary**: `cd src/xdebbie && cargo build --release`
2. **Test bootstrap**: `sudo target/release/xdebbied bootstrap`
3. **Start server**: `sudo target/release/xdebbied serve`
4. **Test client**: `source ~/.xdebbie_env && xdctl host ls`
5. **Deploy CAPX**: See CAPX documentation for Cluster API integration

## More Information

- Full documentation: [BOOTSTRAP.md](src/xdebbie/BOOTSTRAP.md)
- Implementation details: [BOOTSTRAP_IMPLEMENTATION_SUMMARY.md](BOOTSTRAP_IMPLEMENTATION_SUMMARY.md)
- Original design: [XDEBBIED_BOOTSTRAP_DESIGN.md](XDEBBIED_BOOTSTRAP_DESIGN.md)
