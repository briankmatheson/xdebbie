// SPDX-License-Identifier: GPL-3.0-or-later
//! Everything the mesh does to the kernel goes through `NetOps`, so the
//! planner and manager can be tested against `FakeNetOps`. The real
//! implementation shells out to `ip`, `bridge` and `nft` and reads their
//! `-j` JSON, which is how the rest of xdebbied drives libvirt too.

use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    process::Stdio,
    sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::model::{NFT_TABLE, OWNER_ALIAS, parse_tunnel_name};

/// Per-command timeout.
pub const CMD_TIMEOUT: Duration = Duration::from_secs(5);

// ---------- observed state ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneveInfo {
    pub vni: u32,
    pub remote: Option<Ipv4Addr>,
    pub dstport: u16,
    pub ttl: u8,
    pub df_set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeInfo {
    pub stp_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    Geneve(GeneveInfo),
    Bridge(BridgeInfo),
    Other(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub ifindex: u32,
    pub name: String,
    pub mtu: u32,
    pub up: bool, // admin UP flag
    pub master: Option<String>,
    pub alias: Option<String>,
    pub address: Option<String>,
    pub kind: LinkKind,
}

impl Link {
    /// Mesh-owned = `gnv-b<P>` name **and** our alias (section 4.4).
    pub fn mesh_owned(&self) -> bool {
        parse_tunnel_name(&self.name).is_some() && self.alias.as_deref() == Some(OWNER_ALIAS)
    }
    pub fn geneve(&self) -> Option<&GeneveInfo> {
        match &self.kind {
            LinkKind::Geneve(g) => Some(g),
            _ => None,
        }
    }
    pub fn is_bridge(&self) -> bool {
        matches!(self.kind, LinkKind::Bridge(_))
    }
}

/// Bridge port flags we manage (section 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortFlags {
    pub isolated: bool,
    pub learning: bool,
    pub flood: bool,
    pub mcast_flood: bool,
    pub bcast_flood: bool,
    pub neigh_suppress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgePort {
    pub name: String,
    pub master: String,
    pub flags: PortFlags,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FdbEntry {
    pub mac: String,
    pub dev: String,
    pub is_static: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInfo {
    pub dev: String,
    pub prefsrc: Option<Ipv4Addr>,
}

/// Snapshot of what matters for one reconcile pass.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub links: Vec<Link>,
    pub ports: Vec<BridgePort>,
    pub fdb: Vec<FdbEntry>,
}

impl Observed {
    pub fn link(&self, name: &str) -> Option<&Link> {
        self.links.iter().find(|l| l.name == name)
    }
    pub fn port(&self, name: &str) -> Option<&BridgePort> {
        self.ports.iter().find(|p| p.name == name)
    }
}

// ---------- actions ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateTunnel { dev: String, vni: u32, remote: Ipv4Addr, dstport: u16 },
    DeleteTunnel { dev: String },
    SetAlias { dev: String },
    SetMtu { dev: String, mtu: u16 },
    SetMaster { dev: String, bridge: String },
    SetPortFlags { dev: String, flags: PortFlags },
    SetUp { dev: String },
    SetDown { dev: String },
    FdbAdd { mac: String, dev: String, bridge: String },
    FdbDel { mac: String, dev: String, bridge: String },
    SetBridgeMtu { bridge: String, mtu: u16 },
}

impl Action {
    /// Label for logs and the `xdebbie_mesh_reconcile_actions_total` metric.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::CreateTunnel { .. } => "create_tunnel",
            Action::DeleteTunnel { .. } => "delete_tunnel",
            Action::SetAlias { .. } => "set_alias",
            Action::SetMtu { .. } => "set_mtu",
            Action::SetMaster { .. } => "set_master",
            Action::SetPortFlags { .. } => "set_port_flags",
            Action::SetUp { .. } => "set_up",
            Action::SetDown { .. } => "set_down",
            Action::FdbAdd { .. } => "fdb_add",
            Action::FdbDel { .. } => "fdb_del",
            Action::SetBridgeMtu { .. } => "set_bridge_mtu",
        }
    }
    /// The device this action touches (for per-peer error attribution).
    pub fn dev(&self) -> &str {
        match self {
            Action::CreateTunnel { dev, .. }
            | Action::DeleteTunnel { dev }
            | Action::SetAlias { dev }
            | Action::SetMtu { dev, .. }
            | Action::SetMaster { dev, .. }
            | Action::SetPortFlags { dev, .. }
            | Action::SetUp { dev }
            | Action::SetDown { dev }
            | Action::FdbAdd { dev, .. }
            | Action::FdbDel { dev, .. } => dev,
            Action::SetBridgeMtu { bridge, .. } => bridge,
        }
    }
}

// ---------- trait ----------

pub trait NetOps: Send + Sync {
    fn list_links(&self) -> Result<Vec<Link>>;
    fn list_bridge_ports(&self, bridge: &str) -> Result<Vec<BridgePort>>;
    fn list_fdb(&self, bridge: &str) -> Result<Vec<FdbEntry>>;
    fn route_get(&self, dst: Ipv4Addr) -> Result<RouteInfo>;
    fn local_addrs(&self) -> Result<Vec<Ipv4Addr>>;
    fn geneve_module_present(&self) -> bool;
    fn apply(&self, action: &Action) -> Result<()>;
    fn nft_replace_table(&self, rendered: &str) -> Result<()>;
    fn nft_delete_table(&self) -> Result<()>;
    fn nft_table_exists(&self) -> bool;
    /// rx/tx byte counters for a device (from sysfs). `None` if unavailable.
    fn link_stats(&self, dev: &str) -> Option<(u64, u64)>;

    fn observe(&self, bridge: &str) -> Result<Observed> {
        let links = self.list_links()?;
        let has_bridge = links.iter().any(|l| l.name == bridge && l.is_bridge());
        let (ports, fdb) = if has_bridge {
            (self.list_bridge_ports(bridge)?, self.list_fdb(bridge)?)
        } else {
            (vec![], vec![])
        };
        Ok(Observed { links, ports, fdb })
    }
}

// ---------- JSON parsing (pure, fixture-tested) ----------

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}
fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}
fn b(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}

pub fn parse_links(json: &str) -> Result<Vec<Link>> {
    let arr: Vec<Value> = serde_json::from_str(json).context("parsing `ip -j link` output")?;
    let mut out = Vec::with_capacity(arr.len());
    for v in &arr {
        let Some(name) = s(v, "ifname") else {
            tracing::warn!("ip link entry without ifname: {v}");
            continue;
        };
        let flags: Vec<String> = v
            .get("flags")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let li = v.get("linkinfo");
        let kind = match li.and_then(|l| l.get("info_kind")).and_then(Value::as_str) {
            Some("geneve") => {
                let d = li.and_then(|l| l.get("info_data")).cloned().unwrap_or(Value::Null);
                match (u(&d, "id"), u(&d, "port")) {
                    (Some(id), Some(port)) => LinkKind::Geneve(GeneveInfo {
                        vni: id as u32,
                        remote: s(&d, "remote").and_then(|r| r.parse().ok()),
                        dstport: port as u16,
                        ttl: u(&d, "ttl").unwrap_or(0) as u8,
                        df_set: s(&d, "df").as_deref() == Some("set"),
                    }),
                    _ => {
                        tracing::warn!(dev = %name, "geneve link with unexpected info_data: {d}");
                        LinkKind::Unknown
                    }
                }
            }
            Some("bridge") => {
                let d = li.and_then(|l| l.get("info_data")).cloned().unwrap_or(Value::Null);
                LinkKind::Bridge(BridgeInfo {
                    stp_enabled: u(&d, "stp_state").map(|x| x != 0).unwrap_or(false),
                })
            }
            Some(k) => LinkKind::Other(k.to_owned()),
            None => LinkKind::Other(String::new()),
        };
        out.push(Link {
            ifindex: u(v, "ifindex").unwrap_or(0) as u32,
            name,
            mtu: u(v, "mtu").unwrap_or(0) as u32,
            up: flags.iter().any(|f| f == "UP"),
            master: s(v, "master"),
            alias: s(v, "ifalias"),
            address: s(v, "address"),
            kind,
        });
    }
    Ok(out)
}

pub fn parse_bridge_ports(json: &str) -> Result<Vec<BridgePort>> {
    let arr: Vec<Value> = serde_json::from_str(json).context("parsing `bridge -j link` output")?;
    let mut out = Vec::new();
    for v in &arr {
        let (Some(name), Some(master)) = (s(v, "ifname"), s(v, "master")) else {
            continue;
        };
        out.push(BridgePort {
            name,
            master,
            flags: PortFlags {
                isolated: b(v, "isolated").unwrap_or(false),
                learning: b(v, "learning").unwrap_or(true),
                flood: b(v, "flood").unwrap_or(true),
                mcast_flood: b(v, "mcast_flood").unwrap_or(true),
                bcast_flood: b(v, "bcast_flood").unwrap_or(true),
                neigh_suppress: b(v, "neigh_suppress").unwrap_or(false),
            },
        });
    }
    Ok(out)
}

/// Entries on bridge ports (not the bridge's own `self` entries). The
/// kernel lists a `vlan 1` twin for every entry when `default_pvid` is set;
/// they are collapsed to one `(mac, dev)`.
pub fn parse_fdb(json: &str) -> Result<Vec<FdbEntry>> {
    let arr: Vec<Value> = serde_json::from_str(json).context("parsing `bridge -j fdb` output")?;
    let mut set = std::collections::BTreeSet::new();
    for v in &arr {
        let (Some(mac), Some(dev)) = (s(v, "mac"), s(v, "ifname")) else {
            continue;
        };
        if v.get("master").is_none() {
            continue; // `self` entries on the bridge device / vxlan-style remotes
        }
        let state = s(v, "state").unwrap_or_default();
        set.insert(FdbEntry {
            mac,
            dev,
            is_static: state == "static" || state == "permanent",
        });
    }
    Ok(set.into_iter().collect())
}

pub fn parse_route_get(json: &str) -> Result<RouteInfo> {
    let arr: Vec<Value> = serde_json::from_str(json).context("parsing `ip -j route get` output")?;
    let v = arr.first().ok_or_else(|| anyhow!("empty route get result"))?;
    Ok(RouteInfo {
        dev: s(v, "dev").ok_or_else(|| anyhow!("route without dev: {v}"))?,
        prefsrc: s(v, "prefsrc").and_then(|p| p.parse().ok()),
    })
}

pub fn parse_local_addrs(json: &str) -> Result<Vec<Ipv4Addr>> {
    let arr: Vec<Value> = serde_json::from_str(json).context("parsing `ip -j addr` output")?;
    let mut out = Vec::new();
    for v in &arr {
        if let Some(ai) = v.get("addr_info").and_then(Value::as_array) {
            for a in ai {
                if s(a, "family").as_deref() == Some("inet")
                    && let Some(ip) = s(a, "local").and_then(|x| x.parse().ok())
                {
                    out.push(ip);
                }
            }
        }
    }
    Ok(out)
}

// ---------- real implementation ----------

#[derive(Debug, Clone)]
pub struct IpCliNetOps {
    pub ip: String,
    pub bridge: String,
    pub nft: String,
}

impl Default for IpCliNetOps {
    fn default() -> Self {
        Self {
            ip: "ip".into(),
            bridge: "bridge".into(),
            nft: "nft".into(),
        }
    }
}

#[derive(Debug)]
pub struct CmdError {
    pub cmd: String,
    pub status: Option<i32>,
    pub stderr: String,
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(c) => write!(f, "`{}` exited {}: {}", self.cmd, c, self.stderr.trim()),
            None => write!(f, "`{}` failed: {}", self.cmd, self.stderr.trim()),
        }
    }
}

impl std::error::Error for CmdError {}

fn run(bin: &str, args: &[&str], stdin: Option<&str>) -> Result<String> {
    use std::io::Write;
    let cmdline = format!("{bin} {}", args.join(" "));
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CmdError { cmd: cmdline.clone(), status: None, stderr: e.to_string() })?;
    if let Some(input) = stdin
        && let Some(mut si) = child.stdin.take()
    {
        let _ = si.write_all(input.as_bytes());
    }
    // Poll with a deadline; these commands normally finish in milliseconds.
    let deadline = std::time::Instant::now() + CMD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CmdError { cmd: cmdline, status: None, stderr: "timed out after 5s".into() }.into());
            }
            Err(e) => return Err(CmdError { cmd: cmdline, status: None, stderr: e.to_string() }.into()),
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(CmdError {
            cmd: cmdline,
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn onoff(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

impl IpCliNetOps {
    fn ip(&self, args: &[&str]) -> Result<String> {
        run(&self.ip, args, None)
    }
    fn br(&self, args: &[&str]) -> Result<String> {
        run(&self.bridge, args, None)
    }
}

impl NetOps for IpCliNetOps {
    fn list_links(&self) -> Result<Vec<Link>> {
        parse_links(&self.ip(&["-j", "-d", "link", "show"])?)
    }
    fn list_bridge_ports(&self, bridge: &str) -> Result<Vec<BridgePort>> {
        let all = parse_bridge_ports(&self.br(&["-j", "-d", "link", "show"])?)?;
        Ok(all.into_iter().filter(|p| p.master == bridge).collect())
    }
    fn list_fdb(&self, bridge: &str) -> Result<Vec<FdbEntry>> {
        parse_fdb(&self.br(&["-j", "fdb", "show", "br", bridge])?)
    }
    fn route_get(&self, dst: Ipv4Addr) -> Result<RouteInfo> {
        parse_route_get(&self.ip(&["-j", "route", "get", &dst.to_string()])?)
    }
    fn local_addrs(&self) -> Result<Vec<Ipv4Addr>> {
        parse_local_addrs(&self.ip(&["-j", "-4", "addr", "show"])?)
    }
    fn geneve_module_present(&self) -> bool {
        std::path::Path::new("/sys/module/geneve").exists()
    }
    fn apply(&self, a: &Action) -> Result<()> {
        match a {
            Action::CreateTunnel { dev, vni, remote, dstport } => {
                let (vni, remote, dstport) = (vni.to_string(), remote.to_string(), dstport.to_string());
                self.ip(&[
                    "link", "add", dev, "type", "geneve", "id", &vni, "remote", &remote, "dstport", &dstport, "ttl",
                    "64", "df", "set",
                ])?;
            }
            Action::DeleteTunnel { dev } => {
                self.ip(&["link", "del", dev])?;
            }
            Action::SetAlias { dev } => {
                self.ip(&["link", "set", dev, "alias", OWNER_ALIAS])?;
            }
            Action::SetMtu { dev, mtu } => {
                self.ip(&["link", "set", dev, "mtu", &mtu.to_string()])?;
            }
            Action::SetMaster { dev, bridge } => {
                self.ip(&["link", "set", dev, "master", bridge])?;
            }
            Action::SetPortFlags { dev, flags: f } => {
                self.br(&[
                    "link", "set", "dev", dev,
                    "isolated", onoff(f.isolated),
                    "learning", onoff(f.learning),
                    "flood", onoff(f.flood),
                    "mcast_flood", onoff(f.mcast_flood),
                    "bcast_flood", onoff(f.bcast_flood),
                    "neigh_suppress", onoff(f.neigh_suppress),
                ])?;
            }
            Action::SetUp { dev } => {
                self.ip(&["link", "set", dev, "up"])?;
            }
            Action::SetDown { dev } => {
                self.ip(&["link", "set", dev, "down"])?;
            }
            Action::FdbAdd { mac, dev, .. } => {
                self.br(&["fdb", "replace", mac, "dev", dev, "master", "static"])?;
            }
            Action::FdbDel { mac, dev, .. } => {
                self.br(&["fdb", "del", mac, "dev", dev, "master"])?;
            }
            Action::SetBridgeMtu { bridge, mtu } => {
                self.ip(&["link", "set", bridge, "mtu", &mtu.to_string()])?;
            }
        }
        Ok(())
    }
    fn nft_replace_table(&self, rendered: &str) -> Result<()> {
        // `nft -f` with a `table` block replaces the table's contents
        // atomically when preceded by a flush of that table only.
        let script = format!("table inet {NFT_TABLE} {{}}\nflush table inet {NFT_TABLE}\n{rendered}");
        run(&self.nft, &["-f", "-"], Some(&script)).map(|_| ())
    }
    fn nft_delete_table(&self) -> Result<()> {
        match run(&self.nft, &["delete", "table", "inet", NFT_TABLE], None) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("No such file or directory") => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn nft_table_exists(&self) -> bool {
        run(&self.nft, &["list", "table", "inet", NFT_TABLE], None).is_ok()
    }
    fn link_stats(&self, dev: &str) -> Option<(u64, u64)> {
        let rd = |f: &str| {
            std::fs::read_to_string(format!("/sys/class/net/{dev}/statistics/{f}"))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        };
        Some((rd("rx_bytes")?, rd("tx_bytes")?))
    }
}

// ---------- fake ----------

/// In-memory kernel. Links, ports and FDB live in maps; `apply` mutates
/// them the way the real kernel would, so the planner can be run against
/// its own output to prove idempotence.
#[derive(Debug, Default)]
pub struct FakeNetOps {
    pub state: Mutex<FakeState>,
}

#[derive(Debug, Default, Clone)]
pub struct FakeState {
    pub links: BTreeMap<String, Link>,
    pub ports: BTreeMap<String, BridgePort>,
    pub fdb: Vec<FdbEntry>,
    pub routes: BTreeMap<Ipv4Addr, RouteInfo>,
    pub default_route_dev: Option<String>,
    pub local_addrs: Vec<Ipv4Addr>,
    pub geneve_module: bool,
    pub nft_table: Option<String>,
    pub applied: Vec<Action>,
    pub next_ifindex: u32,
    /// Make a specific action kind fail (for error-path tests).
    pub fail_on: Option<&'static str>,
}

impl FakeNetOps {
    pub fn new() -> Self {
        let f = Self::default();
        {
            let mut st = f.state.lock().unwrap();
            st.geneve_module = true;
            st.next_ifindex = 10;
        }
        f
    }
    pub fn add_link(&self, l: Link) {
        let mut st = self.state.lock().unwrap();
        let mut l = l;
        if l.ifindex == 0 {
            l.ifindex = st.next_ifindex;
            st.next_ifindex += 1;
        }
        st.links.insert(l.name.clone(), l);
    }
    pub fn add_bridge(&self, name: &str, mtu: u32, mac: &str) {
        self.add_link(Link {
            ifindex: 0,
            name: name.into(),
            mtu,
            up: true,
            master: None,
            alias: None,
            address: Some(mac.into()),
            kind: LinkKind::Bridge(BridgeInfo::default()),
        });
    }
    pub fn add_underlay(&self, name: &str, mtu: u32, addr: Ipv4Addr) {
        self.add_link(Link {
            ifindex: 0,
            name: name.into(),
            mtu,
            up: true,
            master: None,
            alias: None,
            address: None,
            kind: LinkKind::Other("veth".into()),
        });
        let mut st = self.state.lock().unwrap();
        st.local_addrs.push(addr);
        st.default_route_dev = Some(name.into());
    }
    pub fn set_route(&self, dst: Ipv4Addr, dev: &str) {
        self.state.lock().unwrap().routes.insert(dst, RouteInfo { dev: dev.into(), prefsrc: None });
    }
    pub fn applied(&self) -> Vec<Action> {
        self.state.lock().unwrap().applied.clone()
    }
    pub fn clear_applied(&self) {
        self.state.lock().unwrap().applied.clear();
    }
    pub fn snapshot(&self) -> FakeState {
        self.state.lock().unwrap().clone()
    }
}

impl NetOps for FakeNetOps {
    fn list_links(&self) -> Result<Vec<Link>> {
        Ok(self.state.lock().unwrap().links.values().cloned().collect())
    }
    fn list_bridge_ports(&self, bridge: &str) -> Result<Vec<BridgePort>> {
        Ok(self.state.lock().unwrap().ports.values().filter(|p| p.master == bridge).cloned().collect())
    }
    fn list_fdb(&self, bridge: &str) -> Result<Vec<FdbEntry>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .fdb
            .iter()
            .filter(|e| st.links.get(&e.dev).and_then(|l| l.master.as_deref()) == Some(bridge))
            .cloned()
            .collect())
    }
    fn route_get(&self, dst: Ipv4Addr) -> Result<RouteInfo> {
        let st = self.state.lock().unwrap();
        if let Some(r) = st.routes.get(&dst) {
            return Ok(r.clone());
        }
        match &st.default_route_dev {
            Some(d) => Ok(RouteInfo { dev: d.clone(), prefsrc: None }),
            None => bail!("no route to {dst}"),
        }
    }
    fn local_addrs(&self) -> Result<Vec<Ipv4Addr>> {
        Ok(self.state.lock().unwrap().local_addrs.clone())
    }
    fn geneve_module_present(&self) -> bool {
        self.state.lock().unwrap().geneve_module
    }
    fn apply(&self, a: &Action) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if st.fail_on == Some(a.kind()) {
            bail!("injected failure for {}", a.kind());
        }
        st.applied.push(a.clone());
        match a {
            Action::CreateTunnel { dev, vni, remote, dstport } => {
                if st.links.contains_key(dev) {
                    bail!("RTNETLINK answers: File exists");
                }
                let idx = st.next_ifindex;
                st.next_ifindex += 1;
                st.links.insert(
                    dev.clone(),
                    Link {
                        ifindex: idx,
                        name: dev.clone(),
                        mtu: 1500,
                        up: false,
                        master: None,
                        alias: None,
                        address: None,
                        kind: LinkKind::Geneve(GeneveInfo {
                            vni: *vni,
                            remote: Some(*remote),
                            dstport: *dstport,
                            ttl: 64,
                            df_set: true,
                        }),
                    },
                );
            }
            Action::DeleteTunnel { dev } => {
                if st.links.remove(dev).is_none() {
                    bail!("Cannot find device \"{dev}\"");
                }
                st.ports.remove(dev);
                st.fdb.retain(|e| &e.dev != dev);
            }
            Action::SetAlias { dev } => {
                st.links.get_mut(dev).ok_or_else(|| anyhow!("no {dev}"))?.alias = Some(OWNER_ALIAS.into());
            }
            Action::SetMtu { dev, mtu } => {
                st.links.get_mut(dev).ok_or_else(|| anyhow!("no {dev}"))?.mtu = *mtu as u32;
            }
            Action::SetMaster { dev, bridge } => {
                st.links.get_mut(dev).ok_or_else(|| anyhow!("no {dev}"))?.master = Some(bridge.clone());
                st.ports.insert(
                    dev.clone(),
                    BridgePort {
                        name: dev.clone(),
                        master: bridge.clone(),
                        flags: PortFlags {
                            isolated: false,
                            learning: true,
                            flood: true,
                            mcast_flood: true,
                            bcast_flood: true,
                            neigh_suppress: false,
                        },
                    },
                );
            }
            Action::SetPortFlags { dev, flags } => {
                st.ports.get_mut(dev).ok_or_else(|| anyhow!("{dev} is not a bridge port"))?.flags = *flags;
            }
            Action::SetUp { dev } => {
                st.links.get_mut(dev).ok_or_else(|| anyhow!("no {dev}"))?.up = true;
            }
            Action::SetDown { dev } => {
                st.links.get_mut(dev).ok_or_else(|| anyhow!("no {dev}"))?.up = false;
            }
            Action::FdbAdd { mac, dev, .. } => {
                if !st.links.contains_key(dev) {
                    bail!("no {dev}");
                }
                st.fdb.retain(|e| !(&e.mac == mac && &e.dev == dev));
                st.fdb.push(FdbEntry { mac: mac.clone(), dev: dev.clone(), is_static: true });
            }
            Action::FdbDel { mac, dev, .. } => {
                st.fdb.retain(|e| !(&e.mac == mac && &e.dev == dev));
            }
            Action::SetBridgeMtu { bridge, mtu } => {
                st.links.get_mut(bridge).ok_or_else(|| anyhow!("no {bridge}"))?.mtu = *mtu as u32;
            }
        }
        Ok(())
    }
    fn nft_replace_table(&self, rendered: &str) -> Result<()> {
        self.state.lock().unwrap().nft_table = Some(rendered.to_owned());
        Ok(())
    }
    fn nft_delete_table(&self) -> Result<()> {
        self.state.lock().unwrap().nft_table = None;
        Ok(())
    }
    fn nft_table_exists(&self) -> bool {
        self.state.lock().unwrap().nft_table.is_some()
    }
    fn link_stats(&self, _dev: &str) -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn parses_ip_link_fixture() {
        let links = parse_links(&fx("ip-link.json")).unwrap();
        let br = links.iter().find(|l| l.name == "br-int").unwrap();
        assert!(br.is_bridge());
        assert_eq!(br.mtu, 1370);
        assert_eq!(br.address.as_deref(), Some("52:54:00:23:00:fe"));
        assert!(matches!(&br.kind, LinkKind::Bridge(b) if !b.stp_enabled));
        let g = links.iter().find(|l| l.name == "gnv-b1").unwrap();
        assert!(g.mesh_owned());
        assert!(g.up);
        assert_eq!(g.master.as_deref(), Some("br-int"));
        let gi = g.geneve().unwrap();
        assert_eq!(gi.vni, 23);
        assert_eq!(gi.dstport, 6081);
        assert_eq!(gi.remote, Some("10.22.1.1".parse().unwrap()));
        assert_eq!(gi.ttl, 64);
        assert!(gi.df_set);
    }

    #[test]
    fn mixed_fixture_distinguishes_foreign_and_down() {
        let links = parse_links(&fx("ip-link-mixed.json")).unwrap();
        let foreign = links.iter().find(|l| l.name == "gnv-b9").unwrap();
        assert!(!foreign.mesh_owned(), "no alias → foreign");
        assert!(foreign.geneve().is_some());
        let down = links.iter().find(|l| l.name == "gnv-b2").unwrap();
        assert!(down.mesh_owned());
        assert!(!down.up);
    }

    #[test]
    fn parses_bridge_ports_fixture() {
        let ports = parse_bridge_ports(&fx("bridge-link.json")).unwrap();
        assert_eq!(ports.len(), 2);
        let p = ports.iter().find(|p| p.name == "gnv-b1").unwrap();
        assert_eq!(p.master, "br-int");
        assert!(p.flags.isolated);
        assert!(p.flags.learning);
        assert!(p.flags.flood);
        assert!(p.flags.bcast_flood);
        assert!(!p.flags.neigh_suppress);
    }

    #[test]
    fn parses_fdb_fixture_collapsing_vlan_twins() {
        let fdb = parse_fdb(&fx("bridge-fdb.json")).unwrap();
        let st: Vec<_> = fdb.iter().filter(|e| e.is_static && e.dev == "gnv-b1" && e.mac == "52:54:00:23:01:05").collect();
        assert_eq!(st.len(), 1, "vlan-1 twin must collapse: {st:?}");
        assert!(fdb.iter().any(|e| e.dev == "gnv-b1" && !e.is_static), "learned entries kept");
        assert!(!fdb.iter().any(|e| e.dev == "br-int" && e.mac.starts_with("33:33")), "self entries dropped");
    }

    #[test]
    fn parses_route_and_addrs() {
        let r = parse_route_get(&fx("route-get.json")).unwrap();
        assert_eq!(r.dev, "ul0");
        assert_eq!(r.prefsrc, Some("10.22.0.1".parse().unwrap()));
        let a = parse_local_addrs(&fx("ip-addr.json")).unwrap();
        assert!(a.contains(&"10.22.0.1".parse().unwrap()));
        assert!(a.contains(&"10.23.0.1".parse().unwrap()));
    }

    #[test]
    fn unexpected_shapes_do_not_panic() {
        assert!(parse_links("[{\"ifindex\":1}]").unwrap().is_empty());
        assert!(parse_links("[{\"ifname\":\"x\",\"linkinfo\":{\"info_kind\":\"geneve\",\"info_data\":{}}}]").unwrap()[0].geneve().is_none());
        assert!(parse_fdb("[]").unwrap().is_empty());
        assert!(parse_route_get("[]").is_err());
        assert!(parse_links("not json").is_err());
    }
}
