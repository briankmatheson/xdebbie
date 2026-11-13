// SPDX-License-Identifier: GPL-3.0-or-later
//! Libvirt resource discovery and automatic creation.

use anyhow::{Context, Result};
use std::process::Command;
use tracing::info;

#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub name: String,
    pub bridge: String,
}

#[derive(Debug, Clone)]
pub struct StoragePoolInfo {
    pub name: String,
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

        pools.push(StoragePoolInfo { name: name.to_string() });
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
/// `mac`: the bridge's MAC. The mesh's derived FDB scheme assumes
/// `52:54:00:23:<box>:fe` (`mesh::bridge_mac`), so a network bootstrap
/// creates gets exactly that instead of libvirt's random pick.
pub fn ensure_network(
    uri: &str,
    name: &str,
    subnet: &str,
    dhcp_start: &str,
    dhcp_end: &str,
    mac: &str,
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
  <mac address='{}'/>
{}  <ip address='{}' netmask='{}'>
{}  </ip>
</network>"#,
        name, bridge_name, mac, dns_tag, gateway, netmask, dhcp_section
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

/// The `<mac address>` of a defined network, if any.
pub fn network_mac(uri: &str, name: &str) -> Option<String> {
    let out = Command::new("virsh").args(["-q", "-c", uri, "net-dumpxml", name]).output().ok()?;
    let xml = String::from_utf8_lossy(&out.stdout);
    let i = xml.find("<mac address='")? + "<mac address='".len();
    let rest = &xml[i..];
    Some(rest[..rest.find('\'')?].to_ascii_lowercase())
}

/// What an existing libvirt network looks like, for deciding whether
/// xdebbied may run DHCP on its bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkShape {
    /// `<dhcp>` present: dnsmasq answers on this bridge already.
    pub has_dhcp: bool,
    /// The `<ip address= netmask=>` as a CIDR network, e.g. `10.23.0.0/16`.
    pub subnet: Option<String>,
}

pub fn network_shape(uri: &str, name: &str) -> Option<NetworkShape> {
    let out = Command::new("virsh").args(["-q", "-c", uri, "net-dumpxml", name]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_network_shape(&String::from_utf8_lossy(&out.stdout)))
}

pub fn parse_network_shape(xml: &str) -> NetworkShape {
    let attr = |tag: &str, key: &str| -> Option<String> {
        let i = xml.find(&format!("<{tag} "))?;
        let elem = &xml[i..i + xml[i..].find('>')?];
        let pat = format!("{key}='");
        let j = elem.find(&pat)? + pat.len();
        Some(elem[j..j + elem[j..].find('\'')?].to_string())
    };
    let subnet = (|| {
        let addr: std::net::Ipv4Addr = attr("ip", "address")?.parse().ok()?;
        let prefix = match (attr("ip", "netmask"), attr("ip", "prefix")) {
            (Some(m), _) => {
                let m: std::net::Ipv4Addr = m.parse().ok()?;
                u32::from(m).count_ones()
            }
            (None, Some(p)) => p.parse().ok()?,
            (None, None) => return None,
        };
        let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
        Some(format!("{}/{}", std::net::Ipv4Addr::from(u32::from(addr) & mask), prefix))
    })();
    NetworkShape { has_dhcp: xml.contains("<dhcp"), subnet }
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

/// This host's fully qualified name. Hosts are named by FQDN everywhere
/// (`[[hosts]].name`, placement pins, `xdctl host ls`), so ask the resolver
/// instead of trusting whichever form /etc/hostname happens to hold.
pub fn get_hostname() -> String {
    if let Ok(out) = Command::new("hostname").arg("-f").output()
        && out.status.success() {
            let fqdn = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !fqdn.is_empty() {
                return fqdn;
            }
        }
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

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn shape_of_our_own_network() {
        let xml = "<network>\n  <name>xdebbie</name>\n  <bridge name='virbr-xdebbie' stp='off' delay='0'/>\n  <mac address='52:54:00:23:01:fe'/>\n  <dns enable='no'/>\n  <ip address='10.23.0.2' netmask='255.255.0.0'/>\n</network>";
        assert_eq!(parse_network_shape(xml), NetworkShape { has_dhcp: false, subnet: Some("10.23.0.0/16".into()) });
    }

    #[test]
    fn shape_of_libvirt_default() {
        let xml = "<network>\n  <name>default</name>\n  <forward mode='nat'/>\n  <bridge name='virbr0' stp='on' delay='0'/>\n  <ip address='192.168.122.1' netmask='255.255.255.0'>\n    <dhcp>\n      <range start='192.168.122.2' end='192.168.122.254'/>\n    </dhcp>\n  </ip>\n</network>";
        assert_eq!(parse_network_shape(xml), NetworkShape { has_dhcp: true, subnet: Some("192.168.122.0/24".into()) });
        let bare = "<network><name>x</name><bridge name='br'/><ip address='10.0.0.1' prefix='8'/></network>";
        assert_eq!(parse_network_shape(bare).subnet.as_deref(), Some("10.0.0.0/8"));
        assert_eq!(parse_network_shape("<network><name>x</name></network>").subnet, None);
    }
}
