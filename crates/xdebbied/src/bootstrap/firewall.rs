// SPDX-License-Identifier: GPL-3.0-or-later
//! Host firewall: restrict inbound TCP to the networks this host is a member
//! of (LAN, WireGuard underlay, mesh overlay). Everything else is dropped.
//!
//! Rules live in a dedicated `XDEBBIE-INPUT` chain that is flushed and
//! rebuilt on every run, so re-running bootstrap after an address change
//! yields the right rule set and ordering. INPUT only gets a single jump to
//! that chain. IPv4 only; ip6tables is not touched.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::process::Command;
use tracing::{info, warn};

const CHAIN: &str = "XDEBBIE-INPUT";

/// IPv4 prefixes (`a.b.c.0/24`) of every global-scope address on this host.
/// Inbound TCP from these networks is allowed; this is what lets peers reach
/// the API (7443), libvirt TLS (16514) and ABE (80) over the LAN, the
/// wg-flock underlay (10.22/16) and the mesh overlay (10.23/16).
fn local_ipv4_prefixes() -> Vec<String> {
    let out = Command::new("ip")
        .args(&["-4", "-o", "addr", "show", "scope", "global"])
        .output();
    let Ok(out) = out else { return vec![] };
    let mut nets: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            f.position(|w| w == "inet")?;
            let (ip, len) = f.next()?.split_once('/')?;
            let ip: Ipv4Addr = ip.parse().ok()?;
            let len: u32 = len.parse().ok()?;
            if len > 32 {
                return None;
            }
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            let net = Ipv4Addr::from(u32::from(ip) & mask);
            Some(format!("{}/{}", net, len))
        })
        .collect();
    nets.sort();
    nets.dedup();
    nets
}

fn iptables(args: &[&str]) -> Result<bool> {
    let out = Command::new("iptables")
        .args(args)
        .output()
        .with_context(|| format!("running iptables {}", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // `-C`/`-N` on a missing rule/chain is an expected, non-fatal "no".
        if !args.first().map_or(false, |a| *a == "-C" || *a == "-N") {
            warn!("iptables {} failed: {}", args.join(" "), err.trim());
        }
        return Ok(false);
    }
    Ok(true)
}

/// Configure the firewall. Idempotent; safe to re-run.
pub fn configure_firewall() -> Result<()> {
    info!("Configuring firewall rules...");

    if !Command::new("which")
        .arg("iptables")
        .output()
        .map_or(false, |o| o.status.success())
    {
        warn!("iptables not found, skipping firewall configuration");
        return Ok(());
    }

    let nets = local_ipv4_prefixes();
    if nets.is_empty() {
        warn!("No global IPv4 addresses found; not installing TCP drop rule");
        return Ok(());
    }

    // Build the chain from scratch so ordering is always correct.
    iptables(&["-N", CHAIN])?; // no-op if it exists
    iptables(&["-F", CHAIN])?;
    iptables(&[
        "-A", CHAIN, "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT",
    ])?;
    iptables(&["-A", CHAIN, "-i", "lo", "-j", "ACCEPT"])?;
    for net in &nets {
        info!("  allow inbound TCP from {}", net);
        iptables(&["-A", CHAIN, "-p", "tcp", "-s", net, "-j", "ACCEPT"])?;
    }
    iptables(&["-A", CHAIN, "-p", "tcp", "-j", "DROP"])?;

    // Single jump from INPUT, at the top.
    if !iptables(&["-C", "INPUT", "-j", CHAIN])? {
        iptables(&["-I", "INPUT", "1", "-j", CHAIN])?;
    }

    info!("Firewall configured: inbound TCP limited to {}", nets.join(", "));
    info!("Rules are not persistent across reboots; bootstrap re-applies them on service start.");
    info!("To persist independently: apt install iptables-persistent && netfilter-persistent save");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_networks_not_hosts() {
        // Can't control `ip` output in a unit test; just check the parsing
        // path produces network addresses when it runs.
        for n in local_ipv4_prefixes() {
            let (ip, len) = n.split_once('/').unwrap();
            let ip: Ipv4Addr = ip.parse().unwrap();
            let len: u32 = len.parse().unwrap();
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            assert_eq!(u32::from(ip) & mask, u32::from(ip), "{} is not a network address", n);
        }
    }
}
