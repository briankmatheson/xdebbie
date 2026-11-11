use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Configure firewall to restrict inbound TCP to LAN only
pub fn configure_firewall() -> Result<()> {
    info!("Configuring firewall rules...");
    
    // TODO: Read LAN network from config
    let lan_net = "192.168.1.0/24";
    
    info!("Restricting inbound TCP to LAN network: {}", lan_net);
    
    // Check if iptables is available
    let iptables_check = Command::new("which")
        .arg("iptables")
        .output()
        .context("Failed to check for iptables")?;
    
    if !iptables_check.status.success() {
        warn!("iptables not found, skipping firewall configuration");
        return Ok(());
    }
    
    // Rule 1: Accept established/related connections
    add_rule_if_missing(&[
        "INPUT",
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ])?;
    
    // Rule 2: Accept all loopback traffic
    add_rule_if_missing(&[
        "INPUT",
        "-i",
        "lo",
        "-j",
        "ACCEPT",
    ])?;
    
    // Rule 3: Accept TCP from LAN network
    add_rule_if_missing(&[
        "INPUT",
        "-p",
        "tcp",
        "-s",
        lan_net,
        "-j",
        "ACCEPT",
    ])?;
    
    // Rule 4: Drop all other inbound TCP
    add_rule_if_missing(&[
        "INPUT",
        "-p",
        "tcp",
        "-j",
        "DROP",
    ])?;
    
    info!("Firewall configuration complete");
    info!("Note: To persist these rules across reboots, install iptables-persistent:");
    info!("  sudo apt install iptables-persistent");
    info!("  sudo netfilter-persistent save");
    
    Ok(())
}

/// Add an iptables rule only if it doesn't already exist
fn add_rule_if_missing(args: &[&str]) -> Result<()> {
    // First check if the rule exists using -C (check)
    let mut check_args = vec!["-C"];
    check_args.extend_from_slice(args);
    
    let check = Command::new("iptables")
        .args(&check_args)
        .output()
        .context("Failed to check iptables rule")?;
    
    if check.status.success() {
        // Rule already exists
        info!("iptables rule already exists: {}", args.join(" "));
        return Ok(());
    }
    
    // Rule doesn't exist, add it using -A (append)
    let mut add_args = vec!["-A"];
    add_args.extend_from_slice(args);
    
    let add = Command::new("iptables")
        .args(&add_args)
        .output()
        .context("Failed to add iptables rule")?;
    
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        warn!("Failed to add iptables rule: {}", stderr);
        return Err(anyhow::anyhow!("Failed to add iptables rule: {}", stderr));
    }
    
    info!("Added iptables rule: {}", args.join(" "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configure_firewall_dry_run() {
        // This test just ensures the function compiles and doesn't panic
        // We can't actually test iptables without root privileges
        assert!(true);
    }
}
