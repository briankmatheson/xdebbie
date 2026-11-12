// SPDX-License-Identifier: GPL-3.0-or-later
//! Host firewall, via ufw. One place decides what this hypervisor accepts:
//!
//! | what | from |
//! |---|---|
//! | ssh (22/tcp) | anywhere — never lock the operator out |
//! | API (`listen` port, 7443) and libvirt TLS (16514) | every network this host has an address on: LAN, WireGuard underlay, mesh overlay |
//! | WireGuard (`[mesh.wireguard].listen_port`) | anywhere — peers may come from the internet |
//! | Geneve (`[mesh].dstport`) | the underlay interface only |
//! | discovery multicast (`multicast_addr` port) | the local networks |
//! | DHCP (67/udp) | the bridge the DHCP responder serves |
//!
//! Default policy is deny inbound, allow outbound. Forwarding is left to ufw's
//! default (deny): libvirt inserts its own NAT/forward rules for the VM bridge
//! ahead of ufw's chains, and bridged VM-to-VM frames never hit FORWARD.
//! Anything a host *also* runs (ABE, DNS, apt proxy …) adds its own `ufw
//! allow` next to the service it belongs to; see simple/bin.
//!
//! Every rule is tagged `comment 'xdebbie …'`. `ufw allow` is idempotent
//! ("Skipping adding existing rule"), so re-running bootstrap — which happens
//! on every service start — is safe. IPv4 and IPv6 alike.

use crate::config::Config;
use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::process::Command;
use tracing::{info, warn};

/// The old iptables chain earlier versions installed; removed on sight.
const LEGACY_CHAIN: &str = "XDEBBIE-INPUT";

/// What to open. Built from the daemon config by [`FirewallSpec::from_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallSpec {
    pub api_port: u16,
    /// `a.b.c.0/n` prefixes of this host's global IPv4 addresses
    pub local_nets: Vec<String>,
    pub dhcp_bridge: Option<String>,
    pub wireguard_port: Option<u16>,
    /// (interface, udp port) Geneve arrives on
    pub geneve: Option<(String, u16)>,
    pub discovery_port: Option<u16>,
}

impl FirewallSpec {
    pub fn from_config(cfg: &Config) -> Self {
        let mesh = cfg.mesh.as_ref().filter(|m| m.enabled);
        Self {
            api_port: cfg.effective_listen().port(),
            local_nets: local_ipv4_prefixes(),
            dhcp_bridge: cfg.network.as_ref().and_then(|_| cfg.dhcp_bridge()),
            wireguard_port: mesh.and_then(|m| m.wireguard.as_ref()).map(|w| w.listen_port),
            geneve: mesh.map(|m| (m.underlay.clone(), m.dstport)),
            discovery_port: if cfg.enable_discovery {
                cfg.multicast_addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
            } else {
                None
            },
        }
    }

    /// Without a readable config: ssh, API and libvirt from local networks.
    pub fn minimal() -> Self {
        Self {
            api_port: crate::config::DEFAULT_PORT,
            local_nets: local_ipv4_prefixes(),
            dhcp_bridge: None,
            wireguard_port: None,
            geneve: None,
            discovery_port: None,
        }
    }

    /// The `ufw` argument lists, in order. Pure, so it can be tested.
    pub fn rules(&self) -> Vec<Vec<String>> {
        let mut r: Vec<Vec<String>> = vec![
            args("default deny incoming"),
            args("default allow outgoing"),
            args("allow proto tcp to any port 22 comment 'xdebbie ssh'"),
        ];
        for net in &self.local_nets {
            r.push(args(&format!(
                "allow proto tcp from {net} to any port {} comment 'xdebbie api'",
                self.api_port
            )));
            r.push(args(&format!(
                "allow proto tcp from {net} to any port 16514 comment 'xdebbie libvirt tls'"
            )));
            if let Some(p) = self.discovery_port {
                r.push(args(&format!(
                    "allow proto udp from {net} to any port {p} comment 'xdebbie discovery'"
                )));
            }
        }
        if let Some(p) = self.wireguard_port {
            r.push(args(&format!("allow proto udp to any port {p} comment 'xdebbie wireguard underlay'")));
        }
        if let Some((iface, p)) = &self.geneve {
            r.push(args(&format!(
                "allow in on {iface} proto udp to any port {p} comment 'xdebbie geneve'"
            )));
        }
        if let Some(br) = &self.dhcp_bridge {
            r.push(args(&format!("allow in on {br} proto udp to any port 67 comment 'xdebbie dhcp'")));
        }
        r
    }
}

/// Split a rule string on spaces, keeping a quoted comment as one argument.
fn args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('\'') {
        out.extend(rest[..i].split_whitespace().map(str::to_string));
        let end = rest[i + 1..].find('\'').map(|e| i + 1 + e).unwrap_or(rest.len());
        out.push(rest[i + 1..end].to_string());
        rest = rest.get(end + 1..).unwrap_or("");
    }
    out.extend(rest.split_whitespace().map(str::to_string));
    out
}

/// IPv4 prefixes (`a.b.c.0/n`) of every global-scope address on this host:
/// the LAN, the wg-flock underlay (10.22/16), the mesh overlay (10.23/16).
pub fn local_ipv4_prefixes() -> Vec<String> {
    let out = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "scope", "global"])
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

fn ufw(argv: &[String]) -> Result<()> {
    let out = Command::new("ufw")
        .args(argv)
        .output()
        .with_context(|| format!("running ufw {}", argv.join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        anyhow::bail!("ufw {}: {}{}", argv.join(" "), stdout.trim(), stderr.trim());
    }
    let line = stdout.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if !line.starts_with("Skipping") {
        info!("  ufw {}: {}", argv.join(" "), line);
    }
    Ok(())
}

/// Remove the iptables chain pre-ufw versions installed, if it is there.
fn remove_legacy_chain() {
    let run = |a: &[&str]| Command::new("iptables").args(a).output().map(|o| o.status.success()).unwrap_or(false);
    if run(&["-C", "INPUT", "-j", LEGACY_CHAIN]) {
        run(&["-D", "INPUT", "-j", LEGACY_CHAIN]);
    }
    if run(&["-F", LEGACY_CHAIN]) {
        run(&["-X", LEGACY_CHAIN]);
        info!("  removed legacy iptables chain {LEGACY_CHAIN}");
    }
}

/// Apply `spec` and make sure ufw is enabled. Idempotent.
pub fn configure_firewall(spec: &FirewallSpec) -> Result<()> {
    info!("Configuring firewall (ufw)...");
    if !Command::new("ufw").arg("--version").output().is_ok_and(|o| o.status.success()) {
        warn!("ufw not installed; firewall not configured (apt install ufw)");
        return Ok(());
    }
    remove_legacy_chain();
    for rule in spec.rules() {
        ufw(&rule)?;
    }
    // --force: no interactive "may disrupt existing ssh connections" prompt.
    // ssh was allowed above, before this point.
    ufw(&args("--force enable"))?;
    info!(
        "Firewall: inbound denied except ssh, api:{} and libvirt from {}{}{}",
        spec.api_port,
        spec.local_nets.join(", "),
        spec.wireguard_port.map(|p| format!(", wireguard {p}/udp")).unwrap_or_default(),
        spec.geneve.as_ref().map(|(i, p)| format!(", geneve {p}/udp on {i}")).unwrap_or_default(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_keeps_comment_whole() {
        assert_eq!(
            args("allow in on wg-flock proto udp to any port 6081 comment 'xdebbie geneve'"),
            ["allow", "in", "on", "wg-flock", "proto", "udp", "to", "any", "port", "6081", "comment", "xdebbie geneve"]
        );
        assert_eq!(args("--force enable"), ["--force", "enable"]);
    }

    #[test]
    fn rules_cover_every_service_once() {
        let spec = FirewallSpec {
            api_port: 7443,
            local_nets: vec!["10.22.0.0/16".into(), "192.168.1.0/24".into()],
            dhcp_bridge: Some("virbr-xdebbie".into()),
            wireguard_port: Some(51820),
            geneve: Some(("wg-flock".into(), 6081)),
            discovery_port: Some(7442),
        };
        let rules: Vec<String> = spec.rules().iter().map(|r| r.join(" ")).collect();
        assert_eq!(rules[0], "default deny incoming");
        assert_eq!(rules[1], "default allow outgoing");
        assert!(rules.contains(&"allow proto tcp to any port 22 comment xdebbie ssh".to_string()));
        assert!(rules.contains(&"allow proto tcp from 10.22.0.0/16 to any port 7443 comment xdebbie api".to_string()));
        assert!(rules.contains(&"allow proto tcp from 192.168.1.0/24 to any port 16514 comment xdebbie libvirt tls".to_string()));
        assert!(rules.contains(&"allow proto udp from 192.168.1.0/24 to any port 7442 comment xdebbie discovery".to_string()));
        assert!(rules.contains(&"allow proto udp to any port 51820 comment xdebbie wireguard underlay".to_string()));
        assert!(rules.contains(&"allow in on wg-flock proto udp to any port 6081 comment xdebbie geneve".to_string()));
        assert!(rules.contains(&"allow in on virbr-xdebbie proto udp to any port 67 comment xdebbie dhcp".to_string()));
        assert_eq!(rules.len(), 3 + 2 * 3 + 3);
    }

    #[test]
    fn no_mesh_no_mesh_rules() {
        let spec = FirewallSpec {
            api_port: 7443,
            local_nets: vec![],
            dhcp_bridge: None,
            wireguard_port: None,
            geneve: None,
            discovery_port: None,
        };
        assert_eq!(spec.rules().len(), 3);
    }
}
