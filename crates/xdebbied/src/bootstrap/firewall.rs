// SPDX-License-Identifier: GPL-3.0-or-later
//! Host firewall and VM egress. One place decides what this hypervisor
//! accepts and what its VMs may do.
//!
//! **Inbound** (ufw, default deny). The LAN interface gets nothing except:
//!
//! | what | from |
//! |---|---|
//! | ssh (22/tcp) | anywhere — never lock the operator out |
//! | WireGuard (`[mesh.wireguard].listen_port`) | anywhere — peers may come from the internet |
//! | API (`listen` port, 23443/tcp) | every network this host has an address on: LAN, WireGuard underlay, mesh overlay |
//! | discovery multicast (`multicast_addr` port) | the same local networks — a box not yet meshed finds the cluster over the LAN |
//!
//! Everything else is only accepted on the mesh's own interfaces — the
//! WireGuard underlay and the overlay bridge — never on the LAN:
//!
//! | what | on |
//! |---|---|
//! | libvirt TLS (16514/tcp) | underlay, bridge (libvirtd listens on the bridge address) |
//! | Geneve (`[mesh].dstport`) | underlay |
//! | DHCP (67/udp) | bridge |
//! | abe API and NVMe/TCP (23000-23999/tcp) | storage bridge, from the storage network |
//!
//! **Storage bridge** (`[network] storage_cidr`, `[[hosts]] storage_bridge`,
//! e.g. `virbr-abe` on 10.24/16): VMs reach this host's abe there and
//! nothing else. Only abe's ports are accepted on it, and nothing is
//! forwarded in or out of it (the libvirt network is isolated, and the
//! `xdebbie_host` forward chain drops it too).
//!
//! **VM egress** (`[network]` + `[mesh] box`). Box *N* owns `<cidr>.N.0/24`
//! (bootstrap/addr.rs) for new VMs, but a VM keeps its address when it is
//! placed or moved elsewhere (eyrie-4 on box 5 holds 10.23.0.5, 2026-11-26),
//! so a box forwards for the whole overlay `cidr`: ufw's FORWARD lets
//! it out of the bridge (the libvirt network is `<forward mode='route'/>`,
//! so libvirt forwards too instead of rejecting), and the `xdebbie_host`
//! nftables table
//!
//! - masquerades the overlay behind whatever interface the packet leaves by —
//!   a timesink VM leaves as timesink, a gram VM as gram, VM-to-VM stays on
//!   the mesh L2 and never gets here;
//! - DNATs the VMs' DNS (they are told the bridge, `<cidr>.N.1`) to
//!   `[network] upstream_dns` — the first box's resolver, copied to every
//!   joiner, so all VMs ask the same server;
//! - drops new flows *into* the bridge from any other interface, so a LAN
//!   host with a route to the overlay still cannot reach a VM. With
//!   `[network] mesh_routed = true` the WireGuard underlay's own network
//!   (10.22/16, arriving on the underlay) is let through first, along with
//!   a matching ufw route rule: peers that join routed, not bridged (xdj on
//!   macOS and Windows), reach the VMs. Replies need no NAT: the VMs'
//!   gateway is this box, which routes 10.22/16 into the tunnel.
//!
//! Every ufw rule is tagged `comment 'xdebbie …'` and the comment names its
//! subject (interface, network), so a rule is identified by its comment:
//! tagged rules that are no longer wanted are deleted. Re-running bootstrap
//! — which happens on every service start — converges. IPv4 and IPv6 alike.

use crate::config::Config;
use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};
use tracing::{info, warn};

/// The old iptables chain earlier versions installed; removed on sight.
const LEGACY_CHAIN: &str = "XDEBBIE-INPUT";

/// The nftables table for NAT and VM-side forwarding policy.
pub const NFT_TABLE: &str = "xdebbie_host";

/// Where ip_forward is pinned across reboots.
const SYSCTL_FILE: &str = "/etc/sysctl.d/90-xdebbie-forward.conf";

/// What to open. Built from the daemon config by [`FirewallSpec::from_config`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FirewallSpec {
    pub api_port: u16,
    /// `a.b.c.0/n` prefixes of this host's global IPv4 addresses
    pub local_nets: Vec<String>,
    /// Interfaces the mesh owns: underlay first, then the bridge. The only
    /// places libvirt TLS is accepted.
    pub mesh_ifs: Vec<String>,
    pub dhcp_bridge: Option<String>,
    pub wireguard_port: Option<u16>,
    /// (interface, udp port) Geneve arrives on
    pub geneve: Option<(String, u16)>,
    pub discovery_port: Option<u16>,
    /// This box's VMs: what to NAT and forward for.
    pub egress: Option<Egress>,
    /// The storage bridge, if any.
    pub storage: Option<StorageFw>,
}

/// What the storage bridge lets in: abe, from the storage network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFw {
    pub bridge: String,
    /// `[network] storage_cidr`, e.g. `10.24.0.0/16`.
    pub net: String,
    /// abe's API port(s), from the hosts' `abe` URLs (443 when unset).
    pub abe_api_ports: Vec<u16>,
}

/// abe's NVMe/TCP export range (abe >= 0.5.0).
pub const ABE_EXPORT_PORTS: &str = "23000:23999";

/// The VM side of one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    pub bridge: String,
    /// The bridge's address: VMs' gateway and DNS.
    pub bridge_addr: Ipv4Addr,
    /// The whole overlay (`[network] cidr`), not just this box's /24: a VM
    /// keeps its address wherever it runs (module docs).
    pub vm_net: String,
    /// Where VM DNS goes. Empty: no DNAT, VMs' DNS goes nowhere.
    pub upstream_dns: Vec<Ipv4Addr>,
    /// `[network] mesh_routed`: (underlay interface, underlay network)
    /// whose new flows may enter the bridge.
    pub routed_from: Option<(String, String)>,
}

impl FirewallSpec {
    pub fn from_config(cfg: &Config) -> Self {
        // The effective mesh (on by default, managed WireGuard unless the
        // operator brings an underlay), not just what the file spells out.
        let mesh_cfg = cfg.mesh_config();
        let mesh = mesh_cfg.to_spec();
        let bridge = mesh.bridge();
        let mesh = mesh.enabled.then_some(mesh);
        let underlay_net = {
            let a = u32::from(crate::mesh::model::underlay_addr(mesh_cfg.box_id));
            format!("{}/16", Ipv4Addr::from(a & 0xffff_0000))
        };
        let mut mesh_ifs: Vec<String> = vec![];
        if let Some(m) = &mesh {
            mesh_ifs.push(m.underlay.clone());
        }
        if let Some(b) = cfg.dhcp_bridge().or_else(|| Some(bridge.clone()))
            && !mesh_ifs.contains(&b)
        {
            mesh_ifs.push(b);
        }
        let egress = cfg.network.as_ref().and_then(|n| {
            let bridge = cfg.dhcp_bridge().unwrap_or_else(|| bridge.clone());
            super::addr::box_block(&n.cidr, mesh_cfg.box_id).ok()?;
            Some(Egress {
                bridge,
                bridge_addr: n.gateway(),
                vm_net: n.cidr.to_string(),
                upstream_dns: n.upstream_dns.clone(),
                routed_from: mesh
                    .as_ref()
                    .filter(|_| n.mesh_routed)
                    .map(|m| (m.underlay.clone(), underlay_net.clone())),
            })
        });
        let storage = cfg
            .network
            .as_ref()
            .and_then(|n| n.storage_cidr)
            .and_then(|net| {
                let bridge = cfg.hosts.iter().find_map(|h| h.storage_bridge.clone())?;
                let mut abe_api_ports: Vec<u16> = cfg
                    .hosts
                    .iter()
                    .filter_map(|h| h.abe.as_deref())
                    .filter_map(|u| reqwest::Url::parse(u).ok())
                    .filter_map(|u| u.port_or_known_default())
                    .collect();
                abe_api_ports.sort();
                abe_api_ports.dedup();
                if abe_api_ports.is_empty() {
                    abe_api_ports.push(443);
                }
                Some(StorageFw {
                    bridge,
                    net: net.to_string(),
                    abe_api_ports,
                })
            });
        Self {
            api_port: cfg.effective_listen().port(),
            local_nets: local_ipv4_prefixes(),
            mesh_ifs,
            dhcp_bridge: cfg.network.as_ref().and_then(|_| cfg.dhcp_bridge()),
            wireguard_port: mesh
                .as_ref()
                .and_then(|m| m.wireguard.as_ref())
                .map(|w| w.listen_port),
            geneve: mesh.as_ref().map(|m| (m.underlay.clone(), m.dstport)),
            discovery_port: if cfg.enable_discovery {
                cfg.multicast_addr
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse().ok())
            } else {
                None
            },
            egress,
            storage,
        }
    }

    /// Without a readable config: ssh and the API from local networks.
    pub fn minimal() -> Self {
        Self {
            api_port: crate::config::DEFAULT_PORT,
            local_nets: local_ipv4_prefixes(),
            ..Default::default()
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
                "allow proto tcp from {net} to any port {} comment 'xdebbie api from {net}'",
                self.api_port
            )));
            if let Some(p) = self.discovery_port {
                r.push(args(&format!("allow proto udp from {net} to any port {p} comment 'xdebbie discovery from {net}'")));
            }
        }
        if let Some(p) = self.wireguard_port {
            r.push(args(&format!(
                "allow proto udp to any port {p} comment 'xdebbie wireguard underlay'"
            )));
        }
        for iface in &self.mesh_ifs {
            r.push(args(&format!("allow in on {iface} proto tcp to any port 16514 comment 'xdebbie libvirt tls on {iface}'")));
        }
        if let Some((iface, p)) = &self.geneve {
            r.push(args(&format!(
                "allow in on {iface} proto udp to any port {p} comment 'xdebbie geneve on {iface}'"
            )));
        }
        if let Some(br) = &self.dhcp_bridge {
            r.push(args(&format!(
                "allow in on {br} proto udp to any port 67 comment 'xdebbie dhcp on {br}'"
            )));
        }
        if let Some(e) = &self.egress {
            r.push(args(&format!(
                "route allow in on {} from {} comment 'xdebbie vm egress {} via {}'",
                e.bridge, e.vm_net, e.vm_net, e.bridge
            )));
            if let Some((ul, net)) = &e.routed_from {
                r.push(args(&format!(
                    "route allow in on {ul} out on {} from {net} comment 'xdebbie mesh {net} to vms via {ul}'",
                    e.bridge
                )));
            }
        }
        if let Some(s) = &self.storage {
            let (br, net) = (&s.bridge, &s.net);
            r.push(args(&format!(
                "allow in on {br} proto tcp from {net} to any port {ABE_EXPORT_PORTS} comment 'xdebbie abe nvme/tcp on {br}'"
            )));
            for p in &s.abe_api_ports {
                r.push(args(&format!(
                    "allow in on {br} proto tcp from {net} to any port {p} comment 'xdebbie abe api {p} on {br}'"
                )));
            }
        }
        r
    }

    /// The comments of every rule in [`Self::rules`]: what a converged
    /// firewall carries, and nothing else tagged `xdebbie`.
    fn wanted_comments(&self) -> Vec<String> {
        self.rules().iter().filter_map(|r| comment_of(r)).collect()
    }

    /// The `xdebbie_host` nftables table, or `None` when there is no VM
    /// side (no `[network]`): the table is then deleted.
    pub fn render_nft(&self) -> Option<String> {
        let e = self.egress.as_ref()?;
        let mut out = format!("table ip {NFT_TABLE} {{\n");
        out.push_str(
            "  chain prerouting {\n    type nat hook prerouting priority dstnat; policy accept;\n",
        );
        if let Some(dns) = e.upstream_dns.first() {
            for proto in ["udp", "tcp"] {
                out.push_str(&format!(
                    "    iifname {br} ip daddr {gw} {proto} dport 53 counter dnat to {dns}\n",
                    br = quote(&e.bridge),
                    gw = e.bridge_addr
                ));
            }
        }
        out.push_str("  }\n");
        out.push_str(
            "  chain forward {\n    type filter hook forward priority -10; policy accept;\n",
        );
        if let Some(r) = routed_rule(self) {
            out.push_str(&format!("    {r}\n"));
        }
        // The storage bridge is a dead end: nothing in or out of it is
        // forwarded, whatever routes a guest may set up.
        if let Some(s) = &self.storage {
            out.push_str(&format!(
                "    iifname {br} counter drop\n    oifname {br} counter drop\n",
                br = quote(&s.bridge)
            ));
        }
        out.push_str(&format!(
            "    iifname != {br} oifname {br} ct state new counter drop\n  }}\n",
            br = quote(&e.bridge)
        ));
        out.push_str(
            "  chain postrouting {\n    type nat hook postrouting priority srcnat; policy accept;\n",
        );
        // An upstream on the bridge itself (a DNS service in a VM) would
        // answer the asking VM directly, from its own address instead of the
        // gateway's the VM asked, and the VM drops that. Masquerading the
        // hairpinned query brings the answer back through this box.
        if let Some(dns) = e.upstream_dns.first() {
            for proto in ["udp", "tcp"] {
                out.push_str(&format!(
                    "    iifname {br} oifname {br} ip daddr {dns} {proto} dport 53 counter masquerade\n",
                    br = quote(&e.bridge)
                ));
            }
        }
        out.push_str(&format!(
            "    ip saddr {net} oifname != {br} counter masquerade\n  }}\n",
            net = e.vm_net,
            br = quote(&e.bridge)
        ));
        out.push_str("}\n");
        Some(out)
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Split a rule string on spaces, keeping a quoted comment as one argument.
fn args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('\'') {
        out.extend(rest[..i].split_whitespace().map(str::to_string));
        let end = rest[i + 1..]
            .find('\'')
            .map(|e| i + 1 + e)
            .unwrap_or(rest.len());
        out.push(rest[i + 1..end].to_string());
        rest = rest.get(end + 1..).unwrap_or("");
    }
    out.extend(rest.split_whitespace().map(str::to_string));
    out
}

/// The `comment '…'` of a rule's argument list.
fn comment_of(rule: &[String]) -> Option<String> {
    let i = rule.iter().position(|a| a == "comment")?;
    rule.get(i + 1).cloned()
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

/// The nft accept that lets routed mesh peers into the bridge, tagged
/// [`crate::settings::ROUTED_TAG`] so it can be found and removed alone.
/// `None` without `[network]` or without `mesh_routed`.
pub fn routed_rule(spec: &FirewallSpec) -> Option<String> {
    let e = spec.egress.as_ref()?;
    let (ul, net) = e.routed_from.as_ref()?;
    Some(format!(
        "iifname {} ip saddr {net} oifname {} counter accept comment {}",
        quote(ul),
        quote(&e.bridge),
        quote(crate::settings::ROUTED_TAG)
    ))
}

/// Run one ufw rule (an argument list from [`FirewallSpec::rules`]).
pub fn ufw_run(argv: &[String]) -> Result<String> {
    ufw(argv)
}

fn ufw(argv: &[String]) -> Result<String> {
    let out = Command::new("ufw")
        .args(argv)
        .output()
        .with_context(|| format!("running ufw {}", argv.join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        anyhow::bail!("ufw {}: {}{}", argv.join(" "), stdout.trim(), stderr.trim());
    }
    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if !line.starts_with("Skipping") && argv.first().map(String::as_str) != Some("status") {
        info!("  ufw {}: {}", argv.join(" "), line);
    }
    Ok(stdout)
}

/// `xdebbie`-tagged rules in `ufw status numbered` output whose comment is
/// not in `wanted`: their numbers, highest first, so deleting in order
/// does not renumber what is still to be deleted.
pub fn stale_rule_numbers(status_numbered: &str, wanted: &[String]) -> Vec<u32> {
    let mut stale: Vec<u32> = status_numbered
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix('[')?;
            let (num, rest) = rest.split_once(']')?;
            let num: u32 = num.trim().parse().ok()?;
            let comment = rest.rsplit_once("# ")?.1.trim();
            (comment.starts_with("xdebbie") && !wanted.iter().any(|w| w == comment)).then_some(num)
        })
        .collect();
    stale.sort_unstable_by(|a, b| b.cmp(a));
    stale.dedup();
    stale
}

/// Delete tagged rules this spec no longer wants (an earlier version's
/// shape, a renamed interface, a dropped service).
fn prune_stale(spec: &FirewallSpec) -> Result<()> {
    let status = ufw(&args("status numbered"))?;
    let wanted = spec.wanted_comments();
    for n in stale_rule_numbers(&status, &wanted) {
        if let Err(e) = ufw(&args(&format!("--force delete {n}"))) {
            warn!("  could not delete stale ufw rule {n}: {e:#}");
        }
    }
    Ok(())
}

/// Remove the iptables chain pre-ufw versions installed, if it is there.
fn remove_legacy_chain() {
    let run = |a: &[&str]| {
        Command::new("iptables")
            .args(a)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if run(&["-C", "INPUT", "-j", LEGACY_CHAIN]) {
        run(&["-D", "INPUT", "-j", LEGACY_CHAIN]);
    }
    if run(&["-F", LEGACY_CHAIN]) {
        run(&["-X", LEGACY_CHAIN]);
        info!("  removed legacy iptables chain {LEGACY_CHAIN}");
    }
}

/// VM egress (NAT, DNS forwarding) at daemon start. The nftables table is
/// not saved across a reboot (ufw's rules and the ip_forward sysctl are),
/// so `serve` puts it back from the config -- this used to happen only
/// because the unit ran `bootstrap` before every start. Touches nothing
/// else: no ufw, no packages.
pub fn apply_vm_egress(cfg: &Config) -> Result<()> {
    let spec = FirewallSpec::from_config(cfg);
    if spec.egress.is_some() {
        enable_ip_forward();
    }
    apply_nft(&spec)
}

/// Apply (or drop) the `xdebbie_host` nftables table.
fn apply_nft(spec: &FirewallSpec) -> Result<()> {
    if !Command::new("nft")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        warn!("nft not installed; VM NAT and DNS forwarding not configured (apt install nftables)");
        return Ok(());
    }
    match spec.render_nft() {
        Some(rendered) => {
            // A `table` block adds to an existing table; flushing it first
            // in the same transaction makes this a replace.
            let script =
                format!("table ip {NFT_TABLE} {{}}\nflush table ip {NFT_TABLE}\n{rendered}");
            let mut child = Command::new("nft")
                .args(["-f", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("running nft -f -")?;
            use std::io::Write;
            child
                .stdin
                .take()
                .context("nft stdin")?
                .write_all(script.as_bytes())?;
            let out = child.wait_with_output()?;
            if !out.status.success() {
                anyhow::bail!("nft -f: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            let e = spec.egress.as_ref().expect("render_nft implies egress");
            info!(
                "  nft {NFT_TABLE}: masquerade {} out of {}{}",
                e.vm_net,
                e.bridge,
                match e.upstream_dns.first() {
                    Some(d) => format!(", VM DNS at {} forwarded to {d}", e.bridge_addr),
                    None => ", no upstream_dns: VM DNS is not forwarded".into(),
                }
            );
        }
        None => {
            let out = Command::new("nft")
                .args(["delete", "table", "ip", NFT_TABLE])
                .output()?;
            if out.status.success() {
                info!("  nft {NFT_TABLE}: removed (no [network])");
            }
        }
    }
    Ok(())
}

/// Turn IPv4 forwarding on, now and after reboot. libvirt does this too for
/// a routed network; doing it here means the firewall does not depend on
/// the network being up first.
fn enable_ip_forward() {
    if let Err(e) = std::fs::write("/proc/sys/net/ipv4/ip_forward", "1\n") {
        warn!("  could not enable ip_forward: {e}");
    }
    let want = "# xdebbie: hypervisors forward and NAT for their VMs (bootstrap/firewall.rs)\nnet.ipv4.ip_forward = 1\n";
    if std::fs::read_to_string(SYSCTL_FILE).ok().as_deref() != Some(want)
        && let Err(e) = std::fs::write(SYSCTL_FILE, want)
    {
        warn!("  could not write {SYSCTL_FILE}: {e}");
    }
}

/// Apply `spec` and make sure ufw is enabled. Idempotent.
pub fn configure_firewall(spec: &FirewallSpec) -> Result<()> {
    info!("Configuring firewall (ufw)...");
    if !Command::new("ufw")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        warn!("ufw not installed; firewall not configured (apt install ufw)");
        return Ok(());
    }
    remove_legacy_chain();
    // Stale first, then add: ufw may treat a rule that differs only in its
    // comment as "existing" and keep the old comment, which the prune would
    // then take for stale. Deleting first makes the add real either way.
    // The ssh rule's comment never changes, so it is never in this set.
    prune_stale(spec)?;
    for rule in spec.rules() {
        ufw(&rule)?;
    }
    // --force: no interactive "may disrupt existing ssh connections" prompt.
    // ssh was allowed above, before this point.
    ufw(&args("--force enable"))?;
    info!(
        "Firewall: LAN accepts only ssh{}, api:{} and discovery from {}; libvirt{} only on {}",
        spec.wireguard_port
            .map(|p| format!(", wireguard {p}/udp"))
            .unwrap_or_default(),
        spec.api_port,
        spec.local_nets.join(", "),
        spec.geneve
            .as_ref()
            .map(|(_, p)| format!("/geneve {p}"))
            .unwrap_or_default(),
        spec.mesh_ifs.join(", "),
    );
    if spec.egress.is_some() {
        enable_ip_forward();
    }
    apply_nft(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> FirewallSpec {
        FirewallSpec {
            api_port: 23443,
            local_nets: vec!["10.22.0.0/16".into(), "192.168.1.0/24".into()],
            mesh_ifs: vec!["wg-flock".into(), "virbr-xdebbie".into()],
            dhcp_bridge: Some("virbr-xdebbie".into()),
            wireguard_port: Some(51820),
            geneve: Some(("wg-flock".into(), 6081)),
            discovery_port: Some(23442),
            egress: Some(Egress {
                bridge: "virbr-xdebbie".into(),
                bridge_addr: "10.23.1.1".parse().unwrap(),
                vm_net: "10.23.1.0/24".into(),
                upstream_dns: vec!["192.168.1.1".parse().unwrap()],
                routed_from: None,
            }),
            storage: None,
        }
    }

    #[test]
    fn storage_bridge_takes_abe_only_and_forwards_nothing() {
        let mut s = full();
        assert!(!s.render_nft().unwrap().contains("virbr-abe"));
        s.storage = Some(StorageFw {
            bridge: "virbr-abe".into(),
            net: "10.24.0.0/16".into(),
            abe_api_ports: vec![2],
        });
        let rules: Vec<String> = s.rules().iter().map(|r| r.join(" ")).collect();
        assert!(rules.contains(&"allow in on virbr-abe proto tcp from 10.24.0.0/16 to any port 23000:23999 comment xdebbie abe nvme/tcp on virbr-abe".to_string()), "{rules:#?}");
        assert!(rules.contains(&"allow in on virbr-abe proto tcp from 10.24.0.0/16 to any port 2 comment xdebbie abe api 2 on virbr-abe".to_string()), "{rules:#?}");
        // nothing else is opened on it
        assert_eq!(rules.iter().filter(|r| r.contains("virbr-abe")).count(), 2);
        let nft = s.render_nft().unwrap();
        assert!(nft.contains("iifname \"virbr-abe\" counter drop"), "{nft}");
        assert!(nft.contains("oifname \"virbr-abe\" counter drop"), "{nft}");
    }

    #[test]
    fn mesh_routed_lets_the_underlay_into_the_bridge() {
        let mut s = full();
        assert!(!s.render_nft().unwrap().contains("counter accept"));
        assert!(
            !s.rules()
                .iter()
                .any(|r| r.join(" ").contains("xdebbie mesh"))
        );
        s.egress.as_mut().unwrap().routed_from = Some(("wg-flock".into(), "10.22.0.0/16".into()));
        let nft = s.render_nft().unwrap();
        let accept = nft
            .find("iifname \"wg-flock\" ip saddr 10.22.0.0/16 oifname \"virbr-xdebbie\" counter accept")
            .expect(&nft);
        let drop = nft.find("ct state new counter drop").unwrap();
        assert!(
            accept < drop,
            "the accept must come before the drop:\n{nft}"
        );
        let rules: Vec<String> = s.rules().iter().map(|r| r.join(" ")).collect();
        assert!(rules.contains(
            &"route allow in on wg-flock out on virbr-xdebbie from 10.22.0.0/16 comment xdebbie mesh 10.22.0.0/16 to vms via wg-flock".to_string()
        ), "{rules:#?}");
        assert!(
            s.wanted_comments()
                .iter()
                .any(|c| c == "xdebbie mesh 10.22.0.0/16 to vms via wg-flock")
        );
    }

    fn golden(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn args_keeps_comment_whole() {
        assert_eq!(
            args("allow in on wg-flock proto udp to any port 6081 comment 'xdebbie geneve'"),
            [
                "allow",
                "in",
                "on",
                "wg-flock",
                "proto",
                "udp",
                "to",
                "any",
                "port",
                "6081",
                "comment",
                "xdebbie geneve"
            ]
        );
        assert_eq!(args("--force enable"), ["--force", "enable"]);
    }

    #[test]
    fn lan_gets_only_ssh_wireguard_api_and_discovery() {
        let rules: Vec<String> = full().rules().iter().map(|r| r.join(" ")).collect();
        assert_eq!(rules[0], "default deny incoming");
        assert_eq!(rules[1], "default allow outgoing");
        assert!(rules.contains(&"allow proto tcp to any port 22 comment xdebbie ssh".to_string()));
        assert!(rules.contains(
            &"allow proto udp to any port 51820 comment xdebbie wireguard underlay".to_string()
        ));
        assert!(rules.contains(&"allow proto tcp from 192.168.1.0/24 to any port 23443 comment xdebbie api from 192.168.1.0/24".to_string()));
        assert!(rules.contains(&"allow proto tcp from 10.22.0.0/16 to any port 23443 comment xdebbie api from 10.22.0.0/16".to_string()));
        // everything else names a mesh interface
        for r in &rules {
            let unrestricted = !r.contains(" in on ") && !r.starts_with("route ");
            if unrestricted {
                assert!(
                    r.starts_with("default ")
                        || r.contains("port 22 ")
                        || r.contains("port 51820 ")
                        || r.contains("port 23443 ")
                        || r.contains("port 23442 "),
                    "LAN-reachable rule that should not be: {r}"
                );
            }
        }
        assert!(rules.contains(&"allow in on wg-flock proto tcp to any port 16514 comment xdebbie libvirt tls on wg-flock".to_string()));
        assert!(rules.contains(&"allow in on virbr-xdebbie proto tcp to any port 16514 comment xdebbie libvirt tls on virbr-xdebbie".to_string()));
        assert!(rules.contains(&"allow proto udp from 192.168.1.0/24 to any port 23442 comment xdebbie discovery from 192.168.1.0/24".to_string()));
        assert!(rules.contains(&"allow proto udp from 10.22.0.0/16 to any port 23442 comment xdebbie discovery from 10.22.0.0/16".to_string()));
        assert!(rules.contains(&"allow in on wg-flock proto udp to any port 6081 comment xdebbie geneve on wg-flock".to_string()));
        assert!(rules.contains(&"allow in on virbr-xdebbie proto udp to any port 67 comment xdebbie dhcp on virbr-xdebbie".to_string()));
        assert!(rules.contains(&"route allow in on virbr-xdebbie from 10.23.1.0/24 comment xdebbie vm egress 10.23.1.0/24 via virbr-xdebbie".to_string()));
        assert!(
            !rules
                .iter()
                .any(|r| r.contains("from 192.168.1.0/24 to any port 16514")),
            "libvirt open to the LAN"
        );
        assert_eq!(rules.len(), 3 + 2 * 2 + 1 + 2 + 1 + 1 + 1);
    }

    #[test]
    fn comments_identify_rules() {
        let spec = full();
        let c = spec.wanted_comments();
        let mut dedup = c.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(c.len(), dedup.len(), "duplicate comments: {c:?}");
        assert_eq!(
            c.len(),
            spec.rules().len() - 2,
            "every rule but the two defaults is tagged"
        );
    }

    #[test]
    fn no_mesh_no_mesh_rules() {
        let spec = FirewallSpec {
            api_port: 23443,
            ..Default::default()
        };
        assert_eq!(spec.rules().len(), 3);
        assert_eq!(spec.render_nft(), None);
    }

    #[test]
    fn stale_rules_are_the_tagged_ones_not_wanted_highest_first() {
        let status = "Status: active\n\n     To                         Action      From\n     --                         ------      ----\n\
            [ 1] 22/tcp                     ALLOW IN    Anywhere                   # xdebbie ssh\n\
            [ 2] 23443/tcp                   ALLOW IN    10.22.0.0/16               # xdebbie api\n\
            [ 3] 16514/tcp                  ALLOW IN    192.168.1.0/24             # xdebbie libvirt tls\n\
            [ 4] 8080/tcp                   ALLOW IN    Anywhere                   # operator's thing\n\
            [ 5] 6081/udp on wg-flock       ALLOW IN    Anywhere                   # xdebbie geneve on wg-flock\n\
            [ 6] 22/tcp (v6)                ALLOW IN    Anywhere (v6)              # xdebbie ssh\n\
            [ 7] 16514/tcp (v6)             ALLOW IN    Anywhere (v6)              # xdebbie libvirt tls\n";
        let wanted: Vec<String> = ["xdebbie ssh", "xdebbie geneve on wg-flock"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(stale_rule_numbers(status, &wanted), [7, 3, 2]);
        assert_eq!(stale_rule_numbers("", &wanted), Vec::<u32>::new());
    }

    #[test]
    fn golden_nft_host_table() {
        assert_eq!(full().render_nft().unwrap(), golden("nft-host-box1.golden"));
    }

    #[test]
    fn no_upstream_dns_no_dnat() {
        let mut spec = full();
        spec.egress.as_mut().unwrap().upstream_dns.clear();
        let t = spec.render_nft().unwrap();
        assert!(!t.contains("dnat"), "{t}");
        assert!(
            t.contains("ip saddr 10.23.1.0/24 oifname != \"virbr-xdebbie\" counter masquerade"),
            "{t}"
        );
        assert!(
            t.contains(
                "iifname != \"virbr-xdebbie\" oifname \"virbr-xdebbie\" ct state new counter drop"
            ),
            "{t}"
        );
    }
}
