// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration file generation for xdebbied.

use std::net::Ipv4Addr;
use anyhow::{Context, Result};
use std::path::Path;
use tracing::info;

use super::libvirt_setup::{get_hostname, LibvirtResources};

/// Generate default configuration TOML from discovered resources
/// `created_subnet`: the subnet of the libvirt network bootstrap created
/// itself (so it is known to have no dnsmasq DHCP). `None` when an existing
/// network was adopted — then `[network]` is emitted commented out, because
/// turning on a second DHCP server on a bridge that already has one is
/// exactly the mistake we want operators to make on purpose, not by default.
#[cfg(test)]
pub fn generate_config(resources: &LibvirtResources, uri: &str, created_subnet: Option<&str>, box_id: u8, join: Option<&str>) -> String {
    generate_config_for(resources, uri, created_subnet, box_id, join, super::DEFAULT_NETWORK_NAME, super::DEFAULT_POOL_NAME, None, &get_hostname(), &[])
}

/// `advertise_host`: what other boxes put in a URL to reach this one — the
/// FQDN, or its announced address when the name does not resolve (see
/// `bootstrap::reach_host`). The host's *name* stays the hostname.
///
/// `upstream_dns`: where this box's VMs' DNS goes (`[network] upstream_dns`),
/// for a first box; a joiner takes the cluster's from `cluster_net`.
///
/// `network_name` / `pool_name`: what bootstrap ensured exists. Their
/// bridge and name go into the config; the first discovered network is
/// only a fallback (libvirt's `default`, often inactive, sorts first).
pub fn generate_config_for(
    resources: &LibvirtResources,
    uri: &str,
    created_subnet: Option<&str>,
    box_id: u8,
    join: Option<&str>,
    network_name: &str,
    pool_name: &str,
    cluster_net: Option<&crate::dhcp::NetworkStatus>,
    advertise_host: &str,
    upstream_dns: &[Ipv4Addr],
) -> String {
    let hostname = get_hostname();
    let ca_url_line = match join {
        Some(u) => format!("ca_url = \"{}\"", u.trim_end_matches('/')),
        None => "# ca_url = \"https://node0.example.net:7443\"".to_string(),
    };
    // Bootstrap generates api-cert.pem/api-key.pem and enables TLS by default.
    // advertise_url uses https:// to match.
    let advertise_url = format!("https://{}:7443", advertise_host);
    let (net_prefix, net_cidr, net_note) = match created_subnet {
        Some(s) => ("", s.to_string(), "# Bootstrap created the libvirt network without a DHCP range; xdebbied\n# allocates each VM an address at creation and answers DHCP for it itself.".to_string()),
        None => (
            "# ",
            "10.23.0.0/16".to_string(),
            "# Commented out because bootstrap adopted an existing libvirt network that\n# has a <dhcp> range, so dnsmasq already answers on its bridge. Remove the\n# range (virsh net-edit), set `cidr` to the bridge's subnet, then uncomment.".to_string(),
        ),
    };

    // Select network and pool: the ones bootstrap ensured, by name.
    let network = resources
        .networks
        .iter()
        .find(|n| n.name == network_name)
        .or_else(|| resources.networks.first())
        .map(|n| n.bridge.clone())
        .unwrap_or_else(|| "virbr-xdebbie".to_string());

    let pool = resources
        .pools
        .iter()
        .find(|p| p.name == pool_name)
        .or_else(|| resources.pools.first())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "xdebbie".to_string());

    let network_section = match cluster_net {
        Some(n) => network_block(n, box_id, &network).0,
        None => {
            // Box `box_id`'s /24 of the overlay (addr.rs). On a subnet with
            // no room for this box the lines are left commented, as
            // placeholders (the daemon's defaults then apply).
            let (gw, start, end, q) = match super::addr::box_block_str(&net_cidr, box_id) {
                Ok(b) => (b.bridge.to_string(), b.pool_start.to_string(), b.pool_end.to_string(), net_prefix),
                Err(_) => ("<this bridge's address>".into(), "<first VM address>".into(), "<last VM address>".into(), "# "),
            };
            let (upstream, up_note) = if upstream_dns.is_empty() {
                ("[]".to_string(), "  # none found in resolv.conf: VMs get no DNS until set")
            } else {
                (format!("[{}]", upstream_dns.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(", ")), "")
            };
            format!(
                "{net_note}\n{p}[network]\n{p}cidr = \"{net_cidr}\"\n\
                 {q}gateway = \"{gw}\"             # this box's bridge: box {box_id} owns <cidr>.{box_id}.0/24\n\
                 {q}dns = [\"{gw}\"]\n\
                 {p}upstream_dns = {upstream}{up_note}   # VM DNS to the gateway is forwarded here\n\
                 {q}pool_start = \"{start}\"\n\
                 {q}pool_end = \"{end}\"\n\
                 {q}reserved = []\n\
                 {p}# bridge = \"{network}\"              # default: mesh bridge, else first host's\n\
                 {p}# lease_secs = 86400\n\
                 {p}# domain = \"vm.example\"\n\
                 {p}# leader_only = false              # true once control planes share state",
                p = net_prefix
            )
        }
    };

    format!(
        r#"# xdebbied configuration
# Auto-generated by xdebbied bootstrap

# API server listen address
listen = "0.0.0.0:7443"

# State persistence file
state_file = "/var/lib/xdebbie/state.json"

# API bearer token file (generated during bootstrap)
token_file = "/etc/xdebbie/token"

# TLS certificate and key — generated by bootstrap, enables HTTPS by default.
# Comment out both to fall back to plain HTTP (not recommended on a LAN).
tls_cert = "/etc/xdebbie/api-cert.pem"
tls_key  = "/etc/xdebbie/api-key.pem"

# Placement policy: "spread" or "binpack"
policy = "spread"

# Reconciliation interval in seconds
reconcile_interval_secs = 5

# Host staleness threshold in seconds
host_stale_secs = 60

# Path to virsh binary
virsh = "virsh"

# NoCloud seed ISO generation command
seed_command = ["cloud-localds", "--network-config={{net}}", "--vendor-data={{vendor}}", "{{out}}", "{{user}}", "{{meta}}"]

# Multicast discovery and leader election (docs/autodiscovery.md). The
# elected leader runs the reconciler and answers every cluster request;
# followers forward to it. No peers = this box leads.
enable_discovery = true
multicast_addr = "239.0.0.42:7442"
# URL peers and xdctl use to reach this box. Auto-detected from the hostname,
# or from this box's address when the hostname does not resolve.
advertise_url = "{advertise_url}"
# The box holding the cluster CA key (set by `bootstrap --join`). Enrolment
# requests and `xdctl approve` that land here are forwarded to it. Leave
# unset on the box that was bootstrapped first.
{ca_url_line}

# Overlay addressing + built-in DHCP (docs/network.md). The scheduler binds an
# address to every VM at creation (`xdctl vm ls` shows it before first boot)
# and the responder on the bridge answers only for those MACs. One /16 per
# overlay, one /24 per box: every box shares `cidr`; box N's bridge is
# <cidr>.N.1 and its VMs get <cidr>.N.2-254. Nothing overlaps, nothing to
# reserve.
{network_section}

# Geneve overlay mesh (on by default; docs/mesh.md). One tunnel per peer is
# enslaved to the overlay bridge. With zero peers this pins the bridge MTU,
# installs the xdebbie_mesh nftables table and brings up the WireGuard
# underlay so another box can enrol this one. Manage peers at runtime with
# `xdctl mesh ...` (API state overrides this section). Removing the whole
# section leaves the mesh on with these same defaults; `enabled = false`
# turns it off.
[mesh]
enabled = true
box = {box_id}                        # this host's box id (0-253): assigned by the cluster CA at enrolment
bridge = "{bridge}"
vni = 23
dstport = 6081
underlay = "wg-flock"          # interface peers are reached through
allow_plain_underlay = false   # true = you bring the underlay; WireGuard then unmanaged
mtu = "auto"                   # or an integer >= 1280 (underlay mtu - 50)
fdb = "derived"                # "derived" (static 52:54:00:23:<box>:xx) or "learn"
firewall = true                # drop UDP dstport arriving on non-underlay ifaces

# xdebbied builds the WireGuard underlay itself: `underlay` is a WireGuard
# link addressed 10.22.<box>.1/16, keyed from /var/lib/xdebbie/wg.key, with
# one WireGuard peer per mesh peer. `xdctl mesh wg` prints this box's public
# key for enrolment elsewhere.
[mesh.wireguard]
listen_port = 51820
mtu = 1420
keepalive = 25

# [[mesh.peers]]
# box = 1
# pubkey = "<peer's public key from `xdctl mesh wg`>"
# endpoint = "gram.example:51820"
# # remote = "10.22.1.1"       # derived from box when WireGuard is managed
# # probe = "10.23.1.1"        # defaults to 10.23.<box>.1 = that box's bridge (derived) / remote (learn)

# Configured hosts
[[hosts]]
name = "{hostname}"
uri = "{uri}"
failure_domain = "default"
bridge = "{bridge}"
storage_pool = "{pool}"
cpu_overcommit = 2.0
mem_overcommit = 1.0
reserved_cpus = 2
reserved_memory_mib = 4096
control_plane_eligible = true
"#,
        advertise_url = advertise_url,
        hostname = hostname,
        uri = uri,
        bridge = network,
        network_section = network_section,
        pool = pool,
    )
}

/// Write configuration file to disk as root:xdebbie 0440.
pub fn write_config(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating config directory")?;
    }
    crate::token::write_group_readable(path, content)
        .with_context(|| format!("writing config to {}", path.display()))?;
    info!("Configuration written to {} (root:xdebbie 0440)", path.display());
    Ok(())
}

/// Does the existing config parse? `Err` carries the TOML error.
pub fn config_parses(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str::<toml::Value>(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(())
}

/// Copy `path` to `path.bak-<timestamp>` and return the copy's path.
pub fn back_up(path: &Path) -> Result<std::path::PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let mut bak = path.as_os_str().to_owned();
    bak.push(format!(".bak-{stamp}"));
    let bak = std::path::PathBuf::from(bak);
    std::fs::copy(path, &bak).with_context(|| format!("backing up {} to {}", path.display(), bak.display()))?;
    Ok(bak)
}

/// Operator settings regeneration must not lose. Scalars replace the
/// template's line (or are inserted before the first table); tables
/// replace the template's own `[network]` / `[[hosts]]` blocks.
const KEEP_SCALARS: &[&str] = &[
    "volume_group",
    "thin_pool",
    "tls_cert",
    "tls_key",
    "advertise_url",
    "ca_url",
    "policy",
    "reconcile_interval_secs",
    "host_stale_secs",
];
const KEEP_TABLES: &[&str] = &["network", "mesh", "hosts"];

/// Regenerated config with the operator's settings from `old` carried over.
/// `old` that does not parse is an error; the caller decides what to do.
#[cfg(test)]
pub fn carry_over(old: &str, generated: &str) -> Result<String> {
    carry_over_except(old, generated, &[])
}

/// `content` with `[mesh] box` set to `box_id`, everything else (comments
/// included) untouched. A `[mesh]` section without a `box` line gets one;
/// no `[mesh]` section at all gets a minimal one appended.
pub fn set_mesh_box(content: &str, box_id: u8) -> String {
    let mut out = Vec::with_capacity(content.lines().count() + 3);
    let mut in_mesh = false;
    let mut seen_mesh = false;
    let mut done = false;
    for l in content.lines() {
        if let Some(name) = table_header(l) {
            if in_mesh && !done {
                out.push(format!("box = {box_id}"));
                done = true;
            }
            in_mesh = name == "mesh";
            seen_mesh |= in_mesh;
            out.push(l.to_string());
            continue;
        }
        if in_mesh && !done {
            let key = l.trim_start().split(['=', '#']).next().unwrap_or("").trim();
            if key == "box" && !l.trim_start().starts_with('#') {
                let comment = l.find('#').map(|i| &l[i..]).unwrap_or("");
                let pad = if comment.is_empty() { "" } else { "   " };
                out.push(format!("box = {box_id}{pad}{comment}"));
                done = true;
                continue;
            }
        }
        out.push(l.to_string());
    }
    if in_mesh && !done {
        out.push(format!("box = {box_id}"));
        done = true;
    }
    if !seen_mesh && !done {
        out.push(String::new());
        out.push("[mesh]".to_string());
        out.push(format!("box = {box_id}"));
    }
    let mut s = out.join("\n");
    if content.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// `content` with the top-level scalar `key = value` set, comments and
/// everything else untouched. A live line is rewritten (keeping its
/// comment); otherwise a commented-out `# key = …` template line is
/// replaced; otherwise the line is inserted before the first table.
pub fn set_top_level(content: &str, key: &str, value: &str) -> String {
    let line = format!("{key} = {value}");
    let key_of = |l: &str| l.trim_start().trim_start_matches('#').trim_start().split(['=', ' ', '\t']).next().unwrap_or("").to_string();
    let lines: Vec<&str> = content.lines().collect();
    let top_end = lines.iter().position(|l| table_header(l).is_some()).unwrap_or(lines.len());
    let live = lines[..top_end].iter().position(|l| !l.trim_start().starts_with('#') && key_of(l) == key);
    let commented = lines[..top_end].iter().position(|l| l.trim_start().starts_with('#') && key_of(l) == key);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match (live, commented) {
        (Some(i), _) => {
            let l = lines[i];
            let comment = l.find('#').map(|c| &l[c..]).unwrap_or("");
            out[i] = if comment.is_empty() { line } else { format!("{line}   {comment}") };
        }
        (None, Some(i)) => out[i] = line,
        (None, None) => out.insert(top_end, line),
    }
    let mut s = out.join("\n");
    if content.ends_with('\n') || out.len() > lines.len() {
        s.push('\n');
    }
    s
}

/// Regenerated config with the operator's settings from `old` carried
/// over, except `fresh` keys, which take the generated value even if the
/// previous config had one (`ca_url` when re-joining a different CA).
/// `old` that does not parse is an error; the caller decides what to do.
pub fn carry_over_except(old: &str, generated: &str, fresh: &[&str]) -> Result<String> {
    let old: toml::Table = toml::from_str(old).context("previous config")?;
    let mut scalars = toml::Table::new();
    let mut tables = toml::Table::new();
    for k in KEEP_SCALARS {
        if fresh.contains(k) {
            continue;
        }
        if let Some(v) = old.get(*k) {
            scalars.insert(k.to_string(), v.clone());
        }
    }
    for k in KEEP_TABLES {
        if let Some(v) = old.get(*k) {
            tables.insert(k.to_string(), v.clone());
        }
    }
    if scalars.is_empty() && tables.is_empty() {
        return Ok(generated.to_string());
    }

    // Drop the template's own blocks for tables we carry over.
    let mut lines: Vec<&str> = vec![];
    let mut skipping = false;
    for l in generated.lines() {
        if let Some(name) = table_header(l) {
            skipping = tables.contains_key(name);
        }
        if !skipping {
            lines.push(l);
        }
    }

    let line_for = |k: &str| -> Result<String> {
        let mut t = toml::Table::new();
        t.insert(k.to_string(), scalars[k].clone());
        Ok(toml::to_string(&t)?.trim_end().to_string())
    };
    let is_key = |l: &str, k: &str| l.starts_with(k) && l[k.len()..].trim_start().starts_with('=');
    let mut pending = vec![];
    for k in scalars.keys() {
        if !lines.iter().any(|l| is_key(l, k)) {
            pending.push(line_for(k)?);
        }
    }

    let mut out: Vec<String> = vec![];
    let mut inserted = false;
    for l in lines {
        if !inserted && table_header(l).is_some() {
            if !pending.is_empty() {
                out.push("# Kept from the previous configuration".into());
                out.append(&mut pending);
                out.push(String::new());
            }
            inserted = true;
        }
        match scalars.keys().find(|k| is_key(l, k)) {
            Some(k) => out.push(line_for(k)?),
            None => out.push(l.to_string()),
        }
    }
    if !pending.is_empty() {
        out.push("# Kept from the previous configuration".into());
        out.append(&mut pending);
    }
    let mut text = out.join("\n");
    text.push('\n');
    if !tables.is_empty() {
        text.push_str("\n# Kept from the previous configuration\n");
        text.push_str(&toml::to_string(&tables)?);
    }
    Ok(text)
}

/// A `[network]` for box `box_id` on the overlay the cluster already runs:
/// same `cidr`, `domain` and lease; this box's own /24 of it (`addr.rs`) —
/// its bridge as gateway, its VM range as pool. DNS is the cluster's unless
/// that was just the CA box's bridge, then it is this box's bridge too.
/// Returns the block and any warnings: the cluster's pool overlapping this
/// box's /24 means the other box was set up by hand on a different scheme
/// and the operator has to narrow it, or two allocators will collide.
pub fn network_block(n: &crate::dhcp::NetworkStatus, box_id: u8, bridge: &str) -> (String, Vec<String>) {
    let mut warnings = vec![];
    let block = match super::addr::box_block(&n.cidr, box_id) {
        Ok(b) => b,
        Err(e) => {
            // No room for this box: emit something the daemon rejects loudly
            // rather than a pool that collides with another box's.
            warnings.push(format!("{e:#}; [network] left without a pool — fix cidr on every box"));
            let first = n.cidr.first_host();
            super::addr::BoxBlock { bridge: first, pool_start: first, pool_end: first }
        }
    };
    let (gw, start, end) = (block.bridge, block.pool_start, block.pool_end);
    if u32::from(n.pool_start) <= u32::from(end) && u32::from(start) <= u32::from(n.pool_end) {
        warnings.push(format!(
            "the cluster's pool {}-{} overlaps this box's /24 {start}-{end}; narrow it on the box that serves it \
             (box 0 should use {}-{}) or the two allocators will hand out the same address",
            n.pool_start,
            n.pool_end,
            Ipv4Addr::from(u32::from(n.cidr.network) + 2),
            Ipv4Addr::from(u32::from(n.cidr.network) + 254),
        ));
    }
    let dns_list: Vec<Ipv4Addr> = if n.dns.is_empty() || n.dns == [n.gateway] { vec![gw] } else { n.dns.clone() };
    let dns = dns_list.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(", ");
    let upstream = n.upstream_dns.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(", ");
    let reserved_s = n.reserved.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(", ");
    let domain = match &n.domain {
        Some(d) => format!("domain = {d:?}\n"),
        None => String::new(),
    };
    let warn_lines = warnings.iter().map(|w| format!("# WARNING: {w}\n")).collect::<String>();
    let block = format!(
        "# Copied from the cluster at join time (`xdebbied bootstrap --join`): same\n\
         # overlay as every other box; box {box_id} owns its /24 of it (bridge .1, VMs .2-.254).\n\
         {warn_lines}\
         [network]\n\
         cidr = \"{}\"\n\
         gateway = \"{gw}\"\n\
         dns = [{dns}]\n\
         upstream_dns = [{upstream}]\n\
         {domain}\
         pool_start = \"{start}\"\n\
         pool_end = \"{end}\"\n\
         reserved = [{reserved_s}]\n\
         bridge = \"{bridge}\"\n\
         lease_secs = {}",
        n.cidr, n.lease_secs
    );
    (block, warnings)
}

/// Whether `content` defines table `name` (live, not commented out).
pub fn has_table(content: &str, name: &str) -> bool {
    content.lines().any(|l| table_header(l) == Some(name))
}

/// `content` with `block` appended as a new top-level table.
pub fn append_table(content: &str, block: &str) -> String {
    let mut out = content.trim_end_matches('\n').to_string();
    out.push_str("\n\n");
    out.push_str(block.trim_end_matches('\n'));
    out.push('\n');
    out
}

/// `[x]`, `[[x]]`, `[x.y]` -> `x`; comments and values -> `None`.
fn table_header(line: &str) -> Option<&str> {
    let l = line.trim_start();
    if !l.starts_with('[') {
        return None;
    }
    let inner = l.trim_start_matches('[');
    let name = inner.split([']', '.']).next()?.trim();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_config() {
        let resources = LibvirtResources {
            networks: vec![super::super::libvirt_setup::NetworkInfo {
                name: "default".to_string(),
                bridge: "virbr0".to_string(),
            }],
            pools: vec![super::super::libvirt_setup::StoragePoolInfo {
                name: "default".to_string(),
            }],
        };

        // the network bootstrap ensured wins over whatever sorts first
        let mut multi = resources.clone();
        multi.networks.insert(0, super::super::libvirt_setup::NetworkInfo { name: "default".into(), bridge: "virbr0".into() });
        multi.networks.push(super::super::libvirt_setup::NetworkInfo { name: "xdebbie".into(), bridge: "virbr-xdebbie".into() });
        let named = generate_config_for(&multi, "qemu:///system", None, 1, None, "xdebbie", "nope", None, "box.example", &[]);
        let parsed: crate::config::Config = toml::from_str(&named).unwrap();
        assert_eq!(parsed.mesh_config().to_spec().bridge(), "virbr-xdebbie", "{named}");
        assert!(named.contains("bridge = \"virbr-xdebbie\""), "{named}");
        // the advertise host is what was passed in, the host's name is still the hostname
        assert_eq!(parsed.advertise_url.as_deref(), Some("https://box.example:7443"), "{named}");
        let by_addr = generate_config_for(&multi, "qemu:///system", Some("10.23.0.0/16"), 1, None, "xdebbie", "nope", None, "192.168.1.164", &["192.168.1.1".parse().unwrap()]);
        let parsed: crate::config::Config = toml::from_str(&by_addr).unwrap();
        assert_eq!(parsed.advertise_url.as_deref(), Some("https://192.168.1.164:7443"), "{by_addr}");
        assert_eq!(parsed.hosts.first().map(|h| h.name.as_str()), Some(get_hostname().as_str()), "{by_addr}");
        let net = parsed.network.as_ref().unwrap();
        assert_eq!(net.upstream_dns.iter().map(|d| d.to_string()).collect::<Vec<_>>(), ["192.168.1.1"], "{by_addr}");
        assert_eq!(net.gateway.map(|g| g.to_string()).as_deref(), Some("10.23.1.1"), "{by_addr}");
        net.validate().unwrap();

        let config = generate_config(&resources, "qemu:///system", Some("10.23.0.0/16"), 2, None);
        assert!(config.contains("\n[network]\ncidr = \"10.23.0.0/16\""), "{config}");
        assert!(config.contains("\ngateway = \"10.23.2.1\""), "{config}");
        assert!(config.contains("\npool_start = \"10.23.2.2\"\npool_end = \"10.23.2.254\""), "{config}");
        // no room on a /24 for box 2: block lines stay commented, file still parses
        let narrow = generate_config(&resources, "qemu:///system", Some("192.168.100.0/24"), 2, None);
        assert!(narrow.contains("\n# gateway = \"<this bridge's address>\""), "{narrow}");
        let _: crate::config::Config = toml::from_str(&narrow).unwrap();
        let adopted = generate_config(&resources, "qemu:///system", None, 0, Some("https://ca:7443/"));
        assert!(adopted.contains("\n# [network]\n# cidr = "), "{adopted}");
        // both must parse as config
        let parsed: crate::config::Config = toml::from_str(&config).unwrap();
        let joined: crate::config::Config = toml::from_str(&adopted).unwrap();
        assert_eq!(parsed.ca_url, None);
        assert_eq!(joined.ca_url.as_deref(), Some("https://ca:7443"));
        // A re-join to another CA wins over the carried-over ca_url.
        let rejoined = carry_over_except(&adopted, &config, &["ca_url"]).unwrap();
        let rejoined: crate::config::Config = toml::from_str(&rejoined).unwrap();
        assert_eq!(rejoined.ca_url, None);
        let kept: crate::config::Config = toml::from_str(&carry_over(&adopted, &config).unwrap()).unwrap();
        assert_eq!(kept.ca_url.as_deref(), Some("https://ca:7443"));
        // the generated [mesh] is live, on the discovered bridge, with managed WireGuard
        let mesh = parsed.mesh_config().to_spec();
        assert!(mesh.enabled && mesh.bridge() == "virbr0" && mesh.wireguard.is_some(), "{config}");
        assert_eq!(mesh.box_id, 2);
        assert!(config.contains("listen = \"0.0.0.0:7443\""));
        assert!(config.contains("bridge = \"virbr0\""));
        assert!(config.contains("storage_pool = \"default\""));
        // advertise_url must be live (not commented out) and use https://
        assert!(config.contains("\nadvertise_url = \"https://"), "advertise_url not set in generated config:\n{config}");
        // tls_cert and tls_key must be live (not commented out)
        assert!(config.contains("\ntls_cert = \"/etc/xdebbie/api-cert.pem\""), "tls_cert not set:\n{config}");
        assert!(config.contains("\ntls_key"), "tls_key not set:\n{config}");
    }

    #[test]
    fn set_mesh_box_rewrites_only_the_box_line() {
        let c = "listen = \"0.0.0.0:7443\"\n\n[mesh]\nenabled = true\nbox = 0   # old comment\nvni = 23\n\n[[mesh.peers]]\nbox = 7\n";
        let out = set_mesh_box(c, 3);
        assert_eq!(out, "listen = \"0.0.0.0:7443\"\n\n[mesh]\nenabled = true\nbox = 3   # old comment\nvni = 23\n\n[[mesh.peers]]\nbox = 7\n");
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(t["mesh"]["box"].as_integer(), Some(3));

        // [mesh] without a box line gets one; no [mesh] at all gets a section.
        let out = set_mesh_box("[mesh]\nenabled = true\n\n[network]\ncidr = \"10.0.0.0/16\"\n", 5);
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(t["mesh"]["box"].as_integer(), Some(5));
        assert_eq!(t["network"]["cidr"].as_str(), Some("10.0.0.0/16"));
        let out = set_mesh_box("listen = \"0.0.0.0:7443\"\n", 9);
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(t["mesh"]["box"].as_integer(), Some(9));
        assert_eq!(t["listen"].as_str(), Some("0.0.0.0:7443"));

        // The generated template round-trips.
        let resources = LibvirtResources { networks: vec![], pools: vec![] };
        let g = generate_config(&resources, "qemu:///system", None, 0, None);
        let t: toml::Table = toml::from_str(&set_mesh_box(&g, 4)).unwrap();
        assert_eq!(t["mesh"]["box"].as_integer(), Some(4));
    }

    #[test]
    fn set_top_level_rewrites_replaces_template_or_inserts() {
        // Live line keeps its comment.
        let c = "listen = \"0.0.0.0:7443\"\nca_url = \"https://old:7443\"   # the CA\n\n[mesh]\nbox = 0\n";
        let out = set_top_level(c, "ca_url", "\"https://new:7443\"");
        assert_eq!(out, "listen = \"0.0.0.0:7443\"\nca_url = \"https://new:7443\"   # the CA\n\n[mesh]\nbox = 0\n");
        // The template's commented-out line is replaced in place.
        let resources = LibvirtResources { networks: vec![], pools: vec![] };
        let g = generate_config(&resources, "qemu:///system", None, 0, None);
        assert!(g.contains("\n# ca_url = "));
        let out = set_top_level(&g, "ca_url", "\"https://ca:7443\"");
        assert!(!out.contains("\n# ca_url = "));
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(t["ca_url"].as_str(), Some("https://ca:7443"));
        assert_eq!(out.lines().count(), g.lines().count());
        // No such line at all: inserted before the first table, not inside one.
        let out = set_top_level("listen = \"0.0.0.0:7443\"\n\n[mesh]\nbox = 0\n", "ca_url", "\"https://ca:7443\"");
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(t["ca_url"].as_str(), Some("https://ca:7443"));
        assert_eq!(t["mesh"]["box"].as_integer(), Some(0));
        assert!(t["mesh"].get("ca_url").is_none());
    }

    #[test]
    fn regeneration_keeps_operator_settings() {
        let resources = LibvirtResources {
            networks: vec![super::super::libvirt_setup::NetworkInfo { name: "default".to_string(), bridge: "virbr0".to_string() }],
            pools: vec![super::super::libvirt_setup::StoragePoolInfo { name: "default".to_string() }],
        };
        let old = r#"
listen = "0.0.0.0:7443"
state_file = "/var/lib/xdebbie/state.json"
policy = "pack"
volume_group = "timesink-vg"
thin_pool = "abe"
advertise_url = "https://timesink.example:7443"

[network]
cidr = "10.23.0.0/16"
leader_only = true

[mesh]
enabled = true
box = 0
underlay = "wg-flock"
allow_plain_underlay = true

[mesh.wireguard]
listen_port = 51820

[[mesh.peers]]
box = 1
pubkey = "k"
endpoint = "gram.example:51820"

[[hosts]]
name = "timesink.example"
uri = "qemu:///system"
abe = "http://10.22.0.1:80"
cpu_overcommit = 4.0
"#;
        // The template emits its own [network] (created subnet) and [[hosts]].
        let generated = generate_config(&resources, "qemu:///system", Some("192.168.100.0/24"), 0, None);
        let merged = carry_over(old, &generated).unwrap();
        let cfg: crate::config::Config = toml::from_str(&merged).unwrap_or_else(|e| panic!("{e}\n{merged}"));

        assert_eq!(cfg.volume_group.as_deref(), Some("timesink-vg"));
        assert_eq!(cfg.thin_pool.as_deref(), Some("abe"));
        assert_eq!(cfg.advertise_url.as_deref(), Some("https://timesink.example:7443"));
        assert!(merged.contains("policy = \"pack\"") && !merged.contains("policy = \"spread\""));
        let net = cfg.network.unwrap();
        assert_eq!(net.cidr.to_string(), "10.23.0.0/16");
        assert!(net.leader_only);
        assert!(!merged.contains("192.168.100.0/24"), "template's [network] dropped:\n{merged}");
        let mesh = cfg.mesh.unwrap();
        assert!(mesh.enabled);
        assert_eq!(mesh.peers.len(), 1);
        assert_eq!(cfg.hosts.len(), 1);
        assert_eq!(cfg.hosts[0].abe.as_deref(), Some("http://10.22.0.1:80"));
        assert_eq!(cfg.hosts[0].cpu_overcommit, 4.0);
        // Generated things still come from the template.
        assert!(merged.contains("token_file = \"/etc/xdebbie/token\""));

        // Nothing to keep: untouched.
        assert_eq!(carry_over("listen = \"1.2.3.4:7443\"\n", &generated).unwrap(), generated);
        assert!(carry_over("not = [toml", &generated).is_err());
    }

    fn cluster_net(cidr: &str, pool: (&str, &str)) -> crate::dhcp::NetworkStatus {
        let s = format!(
            r#"{{"cidr":"{cidr}","gateway":"10.23.0.1","dns":["10.23.0.1"],"pool_start":"{}","pool_end":"{}",
                "reserved":["10.23.0.9"],"upstream_dns":["192.168.1.1"],"lease_secs":86400,"domain":"vm.example",
                "dhcp":{{"enabled":true,"bridge":"virbr-xdebbie","bound":true,"active":true,"leader_only":false,
                "counters":{{"offers":0,"acks":0,"naks":0,"ignored_unknown":0,"ignored_standby":0,"ignored_other":0,"parse_errors":0,"send_errors":0}},"seen":[]}}}}"#,
            pool.0, pool.1
        );
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn network_block_copies_overlay_and_takes_own_slash_24() {
        let n = cluster_net("10.23.0.0/16", ("10.23.0.2", "10.23.0.254"));
        let (block, warnings) = network_block(&n, 2, "virbr-xdebbie");
        assert!(warnings.is_empty(), "{warnings:?}");
        let t: toml::Value = toml::from_str(&block).unwrap();
        let net = &t["network"];
        assert_eq!(net["cidr"].as_str(), Some("10.23.0.0/16"));
        // box 2's own bridge is its VMs' gateway and DNS (the cluster's DNS was just box 0's bridge)
        assert_eq!(net["gateway"].as_str(), Some("10.23.2.1"));
        assert_eq!(net["dns"].as_array().unwrap()[0].as_str(), Some("10.23.2.1"));
        assert_eq!(net["pool_start"].as_str(), Some("10.23.2.2"));
        assert_eq!(net["pool_end"].as_str(), Some("10.23.2.254"));
        assert_eq!(net["domain"].as_str(), Some("vm.example"));
        // the cluster's reservations are kept as-is; nothing is added
        let reserved: Vec<&str> = net["reserved"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(reserved, ["10.23.0.9"]);
        // the cluster's upstream resolver comes along: every box's VMs ask box 0's server
        assert_eq!(net["upstream_dns"].as_array().unwrap()[0].as_str(), Some("192.168.1.1"));
        // and it is a valid [network] for the daemon
        let cfg: crate::config::Config = toml::from_str(&format!("state_file = \"/x\"\n{block}")).unwrap();
        cfg.network.unwrap().validate().unwrap();

        // a real DNS server is kept
        let mut named = cluster_net("10.23.0.0/16", ("10.23.0.2", "10.23.0.254"));
        named.dns = vec!["10.23.0.53".parse().unwrap()];
        let (block, _) = network_block(&named, 1, "br");
        let t: toml::Value = toml::from_str(&block).unwrap();
        assert_eq!(t["network"]["dns"].as_array().unwrap()[0].as_str(), Some("10.23.0.53"));

        // the first box still allocating from the whole range is flagged
        let wide = cluster_net("10.23.0.0/16", ("10.23.0.16", "10.23.255.254"));
        let (_, warnings) = network_block(&wide, 1, "br");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("10.23.0.2-10.23.0.254"), "{}", warnings[0]);

        // no room for this box on a narrow overlay
        let small = cluster_net("192.168.100.0/24", ("192.168.100.2", "192.168.100.254"));
        let (_, warnings) = network_block(&small, 1, "br");
        assert!(warnings.iter().any(|w| w.contains("room for 1 box")), "{warnings:?}");
    }

    #[test]
    fn kept_config_gains_network_once() {
        let kept = "listen = \"0.0.0.0:7443\"\n\n# [network]\n# cidr = \"x\"\n\n[mesh]\nbox = 1\n";
        assert!(!has_table(kept, "network"));
        let n = cluster_net("10.23.0.0/16", ("10.23.0.2", "10.23.0.254"));
        let out = append_table(kept, &network_block(&n, 1, "virbr-xdebbie").0);
        assert!(has_table(&out, "network"));
        let t: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(t["network"]["pool_start"].as_str(), Some("10.23.1.2"));
        assert_eq!(t["network"]["gateway"].as_str(), Some("10.23.1.1"));
        assert_eq!(t["mesh"]["box"].as_integer(), Some(1));
    }

    #[test]
    fn generated_join_config_carries_cluster_network() {
        let resources = LibvirtResources {
            networks: vec![super::super::libvirt_setup::NetworkInfo { name: "xdebbie".into(), bridge: "virbr-xdebbie".into() }],
            pools: vec![],
        };
        let n = cluster_net("10.23.0.0/16", ("10.23.0.2", "10.23.0.254"));
        let g = generate_config_for(&resources, "qemu:///system", None, 3, Some("https://ca:7443"), "xdebbie", "xdebbie", Some(&n), "box.example", &[]);
        let cfg: crate::config::Config = toml::from_str(&g).unwrap();
        let net = cfg.network.unwrap();
        assert_eq!(net.cidr.to_string(), "10.23.0.0/16");
        assert_eq!(net.pool_start.map(|p| p.to_string()).as_deref(), Some("10.23.3.2"));
        assert_eq!(net.pool_end.map(|p| p.to_string()).as_deref(), Some("10.23.3.254"));
        assert_eq!(net.gateway.map(|p| p.to_string()).as_deref(), Some("10.23.3.1"));
        assert!(!g.contains("# [network]"), "{g}");
    }
}
