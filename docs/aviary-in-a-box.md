# Aviary-in-a-Box Integration

## Overview

Aviary-in-a-Box uses XDebbie with a custom network configuration that differs from the default XDebbie bootstrap setup:

| Aspect | Default XDebbie | Aviary-in-a-Box |
|--------|-----------------|-----------------|
| **Network Name** | `xdebbie` | `internal` |
| **Subnet** | 192.168.100.0/24 | 10.23.0.0/16 |
| **Bridge** | `virbr-xdebbie` | `virbr-int` |
| **DNS** | Libvirt dnsmasq | CoreDNS (external) |
| **DHCP** | Libvirt dnsmasq | ISC DHCP Server (external) |

## Why External DNS/DHCP?

Aviary-in-a-Box uses external DNS and DHCP services for better control:

- **CoreDNS**: Provides authoritative DNS for `internal.timesink.mynetworksettings.com` with custom VM hostname resolution
- **ISC DHCP Server**: Allows static IP assignments based on MAC addresses (needed for deterministic VM placement in X9 milestone)

This setup avoids:
- Port 53 conflicts between dnsmasq and CoreDNS
- Difficulty managing static DHCP leases in libvirt XML
- Need to restart networks when changing DHCP assignments

## Network Setup

### Automatic (Recommended)

Use the provided script:

```bash
sudo ~/src/xdebbie/scripts/create-aiab-network.sh
```

This creates the `internal` network with:
- DNS disabled (`<dns enable='no'/>`)
- No DHCP configuration
- NAT forwarding enabled
- Autostart enabled

### Manual

If you need to customize the network:

```bash
# Create network XML
cat > /tmp/internal-network.xml <<EOF
<network>
  <name>internal</name>
  <forward mode='nat'>
    <nat>
      <port start='1024' end='65535'/>
    </nat>
  </forward>
  <bridge name='virbr-int' stp='on' delay='0'/>
  <mac address='52:54:00:ba:c9:c0'/>
  <dns enable='no'/>
  <ip address='10.23.0.1' netmask='255.255.0.0'>
  </ip>
</network>
EOF

# Define and start
sudo virsh -c qemu:///system net-define /tmp/internal-network.xml
sudo virsh -c qemu:///system net-start internal
sudo virsh -c qemu:///system net-autostart internal
```

## Service Configuration

### CoreDNS Setup

Configuration file: `/etc/coredns/Corefile`

```
# Authoritative for internal domain
internal.timesink.mynetworksettings.com {
    hosts /etc/coredns/libvirt.hosts {
        ttl 60
        fallthrough
    }
    whoami
    log
    errors
}

# Forward other queries
. {
    forward . 8.8.8.8 8.8.4.4
    log
    errors
    cache 30
}
```

VM hostnames are managed by `coredns-libvirt-sync.service` which updates `/etc/coredns/libvirt.hosts` from libvirt DHCP leases.

### ISC DHCP Server Setup

Configuration file: `/etc/dhcp/dhcpd.conf`

```
option domain-name "internal.local";
option domain-name-servers 10.23.0.1, 8.8.8.8;

default-lease-time 600;
max-lease-time 7200;

ddns-update-style none;
authoritative;

subnet 10.23.0.0 netmask 255.255.0.0 {
    option routers 10.23.0.1;
    option subnet-mask 255.255.0.0;
    option broadcast-address 10.23.255.255;
    
    # Dynamic range for unknown hosts
    range 10.23.0.100 10.23.0.200;
}

# Static assignments (add as needed)
host node0 {
    hardware ethernet 52:54:00:be:e1:e1;
    fixed-address 10.23.0.10;
    option host-name "node0";
}
```

For deterministic placement (X9 milestone), static assignments use:
- MAC: `52:54:00:23:<box>:<slot>`
- IP: `10.23.<box>.<10+slot>`

### XDebbie Configuration

Configuration file: `/etc/xdebbie/xdebbied.toml`

```toml
[[hosts]]
name = "timesink"
uri = "qemu:///system"
bridge = "virbr-int"          # Use internal network bridge
storage_pool = "default"
# ... other settings
```

## Troubleshooting

### Network Won't Start

**Error:** `dnsmasq: failed to create listening socket for 10.23.0.1: Address already in use`

**Cause:** DNS is enabled in network XML while CoreDNS is running on port 53.

**Fix:** Ensure `<dns enable='no'/>` is in the network XML:

```bash
sudo virsh -c qemu:///system net-dumpxml internal | grep dns
# Should show: <dns enable='no'/>
```

If not, recreate the network:

```bash
sudo virsh -c qemu:///system net-destroy internal
sudo virsh -c qemu:///system net-undefine internal
sudo ~/src/xdebbie/scripts/create-aiab-network.sh
```

### VMs Don't Get IP Addresses

**Check DHCP server:**

```bash
sudo systemctl status isc-dhcp-server
sudo journalctl -u isc-dhcp-server -n 50
```

**Check bridge exists:**

```bash
ip addr show virbr-int
# Should show: inet 10.23.0.1/16
```

**Check DHCP leases:**

```bash
sudo cat /var/lib/dhcp/dhcpd.leases
```

### DNS Resolution Not Working

**Check CoreDNS:**

```bash
sudo systemctl status coredns
sudo journalctl -u coredns -n 50
```

**Test DNS from host:**

```bash
dig @10.23.0.1 test.internal.timesink.mynetworksettings.com
```

**Check VM hostname file:**

```bash
cat /etc/coredns/libvirt.hosts
```

## Code Changes (v0.3.3 → v0.3.4)

The XDebbie bootstrap code was updated to disable DNS by default:

**File:** `crates/xdebbied/src/bootstrap/libvirt_setup.rs`

```rust
// Before: Always enabled DHCP/DNS (caused conflicts)
let xml = format!(r#"
<network>
  <name>{}</name>
  <bridge name='{}' stp='on' delay='0'/>
  <ip address='{}' netmask='{}'>
    <dhcp>
      <range start='{}' end='{}'/>
    </dhcp>
  </ip>
</network>"#, ...);

// After: DNS disabled by default, DHCP optional
let dns_tag = if enable_dns {
    String::new()
} else {
    "  <dns enable='no'/>\n"
};

let dhcp_section = if enable_dhcp {
    format!("<dhcp>...</dhcp>")
} else {
    String::new()
};

let xml = format!(r#"
<network>
  <name>{}</name>
  <bridge name='{}' stp='on' delay='0'/>
{}  <ip address='{}' netmask='{}'>
{}  </ip>
</network>"#, name, bridge_name, dns_tag, network_addr, netmask, dhcp_section);
```

This prevents port 53 conflicts with CoreDNS and allows flexible DHCP configuration.

## Integration with CAPX

When CAPX creates VMs, they automatically use the `virbr-int` bridge configured in `xdebbied.toml`. The external DHCP server assigns IPs, and CoreDNS provides name resolution.

Example VM lifecycle:
1. CAPX requests VM creation via xdebbied API
2. xdebbied creates VM with `virbr-int` bridge
3. VM boots and sends DHCP request
4. ISC DHCP assigns IP (static or dynamic)
5. coredns-libvirt-sync updates DNS records
6. CoreDNS serves VM hostname queries

## Future: Deterministic Placement (X9)

When implementing deterministic slot placement, the workflow will be:

1. CAPX specifies desired slot (e.g., slot 3 on box 1)
2. xdebbied calculates MAC: `52:54:00:23:01:03`
3. ISC DHCP config has static lease: `10.23.1.13` for that MAC
4. VM always gets same IP based on slot
5. Hostname: `b1s3.internal.timesink.mynetworksettings.com`

This requires no changes to CoreDNS or ISC DHCP - just configuration updates.
