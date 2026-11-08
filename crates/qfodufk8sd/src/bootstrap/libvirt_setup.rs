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
pub fn discover_libvirt(uri: &str) -> Result<LibvirtResources> {
    info!("Discovering existing libvirt resources on {}", uri);
    Ok(LibvirtResources {
        networks: discover_networks(uri).unwrap_or_default(),
        pools: discover_storage_pools(uri).unwrap_or_default(),
    })
}

/// Create a libvirt network if it doesn't exist
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

    // Parse subnet to get network address
    let parts: Vec<&str> = subnet.split('/').collect();
    let network_addr = parts[0];
    let prefix = parts.get(1).unwrap_or(&"24");

    // Calculate netmask from prefix
    let netmask = match *prefix {
        "24" => "255.255.255.0",
        "16" => "255.255.0.0",
        "8" => "255.0.0.0",
        _ => "255.255.255.0",
    };

    let bridge_name = format!("virbr-{}", name);

    // Create network XML
    let xml = format!(
        r#"<network>
  <name>{}</name>
  <bridge name='{}' stp='on' delay='0'/>
  <ip address='{}' netmask='{}'>
    <dhcp>
      <range start='{}' end='{}'/>
    </dhcp>
  </ip>
</network>"#,
        name, bridge_name, network_addr, netmask, dhcp_start, dhcp_end
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
