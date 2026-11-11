// SPDX-License-Identifier: GPL-3.0-or-later
//! Libvirt resource discovery and automatic creation.

use anyhow::{Context, Result};
use std::process::Command;
use tracing::info;

#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub name: String,
    pub bridge: String,
    pub subnet: String,
}

#[derive(Debug, Clone)]
pub struct StoragePoolInfo {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct LibvirtResources {
    pub networks: Vec<NetworkInfo>,
    pub pools: Vec<StoragePoolInfo>,
}

/// Discover existing libvirt networks
pub fn discover_networks(uri: &str) -> Result<Vec<NetworkInfo>> {
    let output = Command::new("virsh")
        .args(["-q", "-c", uri, "net-list", "--all", "--name"])
        .output()
        .context("listing libvirt networks")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let mut networks = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }

        // Get bridge name from network
        let bridge_out = Command::new("virsh")
            .args(["-q", "-c", uri, "net-info", name])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).to_string())
                } else {
                    None
                }
            });

        let bridge = bridge_out
            .as_ref()
            .and_then(|info| {
                info.lines()
                    .find(|l| l.starts_with("Bridge:"))
                    .and_then(|l| l.split_whitespace().nth(1))
            })
            .unwrap_or(name)
            .to_string();

        networks.push(NetworkInfo {
            name: name.to_string(),
            bridge,
            subnet: String::new(), // Could parse from net-dumpxml if needed
        });
    }

    Ok(networks)
}

/// Discover existing storage pools
pub fn discover_storage_pools(uri: &str) -> Result<Vec<StoragePoolInfo>> {
    let output = Command::new("virsh")
        .args(["-q", "-c", uri, "pool-list", "--all", "--name"])
        .output()
        .context("listing libvirt storage pools")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let mut pools = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }

        // Get pool path from pool-info
        let path_out = Command::new("virsh")
            .args(["-q", "-c", uri, "pool-dumpxml", name])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).to_string())
                } else {
                    None
                }
            });

        let path = path_out
            .as_ref()
            .and_then(|xml| {
                // Simple extraction of <path>...</path>
                xml.lines()
                    .find(|l| l.trim().starts_with("<path>"))
                    .and_then(|l| {
                        l.split("<path>")
                            .nth(1)
                            .and_then(|s| s.split("</path>").next())
                    })
            })
            .unwrap_or("/var/lib/libvirt/images")
            .trim()
            .to_string();

        pools.push(StoragePoolInfo {
            name: name.to_string(),
            path,
        });
    }

    Ok(pools)
}

/// Discover all libvirt resources
/// Cheap connectivity check: `virsh -c <uri> uri`. Errors carry virsh's stderr.
pub fn probe(uri: &str) -> Result<()> {
    let out = Command::new("virsh")
        .args(["-c", uri, "uri"])
        .output()
        .context("running virsh")?;
    if out.status.success() {
        return Ok(());
    }
    anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim())
}

pub fn discover_libvirt(uri: &str) -> Result<LibvirtResources> {
    info!("Discovering existing libvirt resources on {}", uri);
    Ok(LibvirtResources {
        networks: discover_networks(uri).unwrap_or_default(),
        pools: discover_storage_pools(uri).unwrap_or_default(),
    })
}

/// Create a libvirt network if it doesn't exist
///
/// DNS is disabled by default to avoid port 53 conflicts with:
/// - CoreDNS (used in Aviary-in-a-Box setups)
/// - systemd-resolved
/// - Other custom DNS servers
///
/// Split `a.b.c.d/N` into the libvirt `<ip>` gateway (first usable host,
/// network + 1) and dotted netmask. libvirt's `<ip address>` is the host's
/// own address on the bridge, not the network number.
pub fn subnet_gateway(subnet: &str) -> Result<(String, String)> {
    use std::net::Ipv4Addr;
    let (addr, prefix) = subnet.split_once('/').unwrap_or((subnet, "24"));
    let addr: Ipv4Addr = addr.parse().context("network address")?;
    let prefix: u32 = prefix.parse().context("prefix length")?;
    anyhow::ensure!((8..=30).contains(&prefix), "prefix length must be 8..=30");
    let mask = u32::MAX << (32 - prefix);
    let network = u32::from(addr) & mask;
    let gateway = Ipv4Addr::from(network + 1);
    Ok((gateway.to_string(), Ipv4Addr::from(mask).to_string()))
}

/// DHCP can be disabled by passing empty strings for dhcp_start/dhcp_end.
/// This is useful when using external DHCP servers like ISC DHCP.
pub fn ensure_network(
    uri: &str,
    name: &str,
    subnet: &str,
    dhcp_start: &str,
    dhcp_end: &str,
) -> Result<String> {
    // Check if network already exists
    let exists = Command::new("virsh")
        .args(["-q", "-c", uri, "net-info", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if exists {
        info!("Network '{}' already exists", name);
        // Get bridge name
        let bridge_out = Command::new("virsh")
            .args(["-q", "-c", uri, "net-info", name])
            .output()
            .context("getting network info")?;

        let bridge = String::from_utf8_lossy(&bridge_out.stdout)
            .lines()
            .find(|l| l.starts_with("Bridge:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or(name)
            .to_string();

        return Ok(bridge);
    }

    info!("Creating network '{}' on subnet {}", name, subnet);

    let (gateway, netmask) = subnet_gateway(subnet)
        .with_context(|| format!("invalid network subnet {subnet:?}"))?;

    let bridge_name = format!("virbr-{}", name);

    // Disable DNS by default to avoid conflicts with CoreDNS or other DNS servers
    let enable_dns = false;
    
    // Only include DHCP if both start and end are provided
    let enable_dhcp = !dhcp_start.is_empty() && !dhcp_end.is_empty();

    // Create network XML
    let dns_tag = if enable_dns {
        String::new()
    } else {
        "  <dns enable='no'/>\n".to_string()
    };
    
    let dhcp_section = if enable_dhcp {
        format!(
            "    <dhcp>\n      <range start='{}' end='{}'/>\n    </dhcp>\n",
            dhcp_start, dhcp_end
        )
    } else {
        String::new()
    };

    let xml = format!(
        r#"<network>
  <name>{}</name>
  <bridge name='{}' stp='off' delay='0'/>
{}  <ip address='{}' netmask='{}'>
{}  </ip>
</network>"#,
        name, bridge_name, dns_tag, gateway, netmask, dhcp_section
    );

    // Write XML to temp file
    let xml_path = format!("/tmp/network-{}.xml", name);
    std::fs::write(&xml_path, xml).context("writing network XML")?;

    // Define network
    let define = Command::new("virsh")
        .args(["-c", uri, "net-define", &xml_path])
        .status()
        .context("defining network")?;

    std::fs::remove_file(&xml_path).ok();

    if !define.success() {
        anyhow::bail!("Failed to define network '{}'", name);
    }

    // Start network
    let start = Command::new("virsh")
        .args(["-c", uri, "net-start", name])
        .status()
        .context("starting network")?;

    if !start.success() {
        anyhow::bail!("Failed to start network '{}'", name);
    }

    // Autostart network
    let _ = Command::new("virsh")
        .args(["-c", uri, "net-autostart", name])
        .status();

    info!("Network '{}' created and started (bridge: {})", name, bridge_name);
    Ok(bridge_name)
}

/// Create a directory-based storage pool if it doesn't exist
pub fn ensure_storage_pool(uri: &str, name: &str, path: &str) -> Result<()> {
    // Check if pool already exists
    let exists = Command::new("virsh")
        .args(["-q", "-c", uri, "pool-info", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if exists {
        info!("Storage pool '{}' already exists", name);
        // Ensure it's active
        let _ = Command::new("virsh")
            .args(["-c", uri, "pool-start", name])
            .status();
        return Ok(());
    }

    info!("Creating storage pool '{}' at {}", name, path);

    // Create directory if it doesn't exist
    std::fs::create_dir_all(path).context("creating storage pool directory")?;

    // Create pool XML
    let xml = format!(
        r#"<pool type='dir'>
  <name>{}</name>
  <target>
    <path>{}</path>
    <permissions>
      <mode>0711</mode>
      <owner>0</owner>
      <group>0</group>
    </permissions>
  </target>
</pool>"#,
        name, path
    );

    // Write XML to temp file
    let xml_path = format!("/tmp/pool-{}.xml", name);
    std::fs::write(&xml_path, xml).context("writing pool XML")?;

    // Define pool
    let define = Command::new("virsh")
        .args(["-c", uri, "pool-define", &xml_path])
        .status()
        .context("defining storage pool")?;

    std::fs::remove_file(&xml_path).ok();

    if !define.success() {
        anyhow::bail!("Failed to define storage pool '{}'", name);
    }

    // Start pool
    let start = Command::new("virsh")
        .args(["-c", uri, "pool-start", name])
        .status()
        .context("starting storage pool")?;

    if !start.success() {
        anyhow::bail!("Failed to start storage pool '{}'", name);
    }

    // Autostart pool
    let _ = Command::new("virsh")
        .args(["-c", uri, "pool-autostart", name])
        .status();

    info!("Storage pool '{}' created and started", name);
    Ok(())
}

/// Get hostname
pub fn get_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
mod gateway_tests {
    use super::subnet_gateway;

    #[test]
    fn gateway_is_first_host() {
        assert_eq!(
            subnet_gateway("192.168.100.0/24").unwrap(),
            ("192.168.100.1".into(), "255.255.255.0".into())
        );
        assert_eq!(
            subnet_gateway("10.23.0.0/16").unwrap(),
            ("10.23.0.1".into(), "255.255.0.0".into())
        );
        // a host address inside the subnet still yields the network's .1
        assert_eq!(subnet_gateway("192.168.100.37/24").unwrap().0, "192.168.100.1");
        assert_eq!(subnet_gateway("192.168.100.0").unwrap().0, "192.168.100.1");
        assert!(subnet_gateway("192.168.100.0/31").is_err());
        assert!(subnet_gateway("nope/24").is_err());
    }
}
