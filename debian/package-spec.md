# qfodufk8sd Debian Package Specification

## Package Details

- **Name**: qfodufk8sd
- **Version**: 0.1.1
- **Architecture**: amd64
- **Section**: admin
- **Priority**: optional
- **Maintainer**: Your Name <your.email@example.com>
- **Description**: Self-bootstrapping VM orchestrator for libvirt with cluster support

## Dependencies

### Runtime Dependencies
```
Depends: libvirt-daemon-system,
         libvirt-clients,
         qemu-kvm,
         qemu-utils,
         cloud-image-utils,
         lvm2,
         thin-provisioning-tools,
         ufw,
         systemd
```

> **Note:** `thin-provisioning-tools` is required for LVM thin pool
> activation (`thin_check`). Without it the pool and every thin LV in it stay
> inactive after a reboot, and VMs marked autostart fail to come up.

### Recommended
```
Recommends: bridge-utils
```

### Suggested
```
Suggests: virt-manager
```

## Files to Install

```
/usr/bin/qfodufk8sd                           # Main binary (from target/release/)
/usr/lib/systemd/system/qfodufk8sd.service    # Systemd service
/etc/qfodufk8s/                               # Config directory (created)
/var/lib/qfodufk8s/                           # State directory (created)
/usr/share/doc/qfodufk8sd/                    # Documentation
/usr/share/doc/qfodufk8sd/README.md
/usr/share/doc/qfodufk8sd/BOOTSTRAP.md
/usr/share/doc/qfodufk8sd/changelog.gz
/usr/share/doc/qfodufk8sd/copyright
```

## Systemd Service

File: `/usr/lib/systemd/system/qfodufk8sd.service`

```ini
[Unit]
Description=qfodufk8s VM Orchestrator
Documentation=https://github.com/yourusername/qfodufk8s
After=network-online.target libvirtd.service multi-user.target
Wants=network-online.target
Requires=libvirtd.service

[Service]
Type=notify
ExecStartPre=/usr/bin/qfodufk8sd bootstrap
ExecStart=/usr/bin/qfodufk8sd serve
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal
SyslogIdentifier=qfodufk8sd

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/etc/qfodufk8s /var/lib/qfodufk8s

[Install]
WantedBy=multi-user.target
```

## Pre/Post Install Scripts

### postinst (after installation)
```bash
#!/bin/sh
set -e

case "$1" in
    configure)
        # Create user if doesn't exist
        if ! getent passwd qfodufk8s > /dev/null; then
            adduser --system --group --home /var/lib/qfodufk8s \
                    --no-create-home --disabled-login qfodufk8s
        fi
        
        # Create directories
        mkdir -p /etc/qfodufk8s
        mkdir -p /var/lib/qfodufk8s
        
        # Set permissions
        chown -R qfodufk8s:qfodufk8s /etc/qfodufk8s
        chown -R qfodufk8s:qfodufk8s /var/lib/qfodufk8s
        chmod 755 /etc/qfodufk8s
        chmod 755 /var/lib/qfodufk8s
        
        # Add qfodufk8s user to libvirt group
        usermod -a -G libvirt qfodufk8s || true
        
        # Reload systemd
        systemctl daemon-reload
        
        # Enable service
        systemctl enable qfodufk8sd.service
        
        # Run bootstrap on first install
        if [ ! -f /etc/qfodufk8s/qfodufk8sd.toml ]; then
            echo "Running initial bootstrap..."
            /usr/bin/qfodufk8sd bootstrap || true
        fi
        
        # Start service
        systemctl start qfodufk8sd.service || true
        ;;
esac

exit 0
```

### prerm (before removal)
```bash
#!/bin/sh
set -e

case "$1" in
    remove|deconfigure)
        systemctl stop qfodufk8sd.service || true
        systemctl disable qfodufk8sd.service || true
        ;;
esac

exit 0
```

### postrm (after removal)
```bash
#!/bin/sh
set -e

case "$1" in
    purge)
        # Remove configuration
        rm -rf /etc/qfodufk8s
        
        # Remove state
        rm -rf /var/lib/qfodufk8s
        
        # Remove user
        deluser qfodufk8s || true
        ;;
esac

exit 0
```

## Build Process

Uses `cargo-deb` for building:

```bash
# Install cargo-deb
cargo install cargo-deb

# Build package
cargo deb --package qfodufk8sd

# Output: target/debian/qfodufk8sd_0.1.1_amd64.deb
```

## Installation

```bash
# Install
sudo dpkg -i qfodufk8sd_0.1.1_amd64.deb

# Fix dependencies if needed
sudo apt-get install -f

# Check status
systemctl status qfodufk8sd

# View logs
journalctl -u qfodufk8sd -f
```

## Upgrade

```bash
# Upgrade
sudo dpkg -i qfodufk8sd_0.1.2_amd64.deb

# Service automatically restarts
```

## Removal

```bash
# Remove but keep config
sudo apt-get remove qfodufk8sd

# Remove including config
sudo apt-get purge qfodufk8sd
```

## Configuration After Install

The service automatically runs bootstrap on first install. Users can:

```bash
# View config
sudo cat /etc/qfodufk8s/qfodufk8sd.toml

# Edit if needed
sudo editor /etc/qfodufk8s/qfodufk8sd.toml

# Restart service
sudo systemctl restart qfodufk8sd
```

## Network Waiting

The systemd service uses:
- `After=network-online.target` - Wait for network to be fully up
- `Wants=network-online.target` - Start network-online.target if not active
- `After=libvirtd.service` - Wait for libvirt
- `Requires=libvirtd.service` - Ensure libvirt is running

This ensures all dependencies are ready before starting.
