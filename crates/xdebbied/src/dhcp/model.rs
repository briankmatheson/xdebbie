// SPDX-License-Identifier: GPL-3.0-or-later
//! `[network]` configuration, the overlay address pool and the allocator.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::str::FromStr;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use xdebbie_core::model::Vm;

pub const DEFAULT_LEASE_SECS: u32 = 86_400;
/// Addresses below `network + POOL_SKIP` are left for gateways, bridge
/// addresses of other boxes and hand-assigned infrastructure.
pub const POOL_SKIP: u32 = 16;

/// An IPv4 prefix, parsed from `a.b.c.d/n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    pub network: Ipv4Addr,
    pub prefix: u8,
}

impl Cidr {
    pub fn new(addr: Ipv4Addr, prefix: u8) -> Result<Self> {
        if !(8..=30).contains(&prefix) {
            bail!("prefix length {prefix} not in 8..=30");
        }
        let m = Self::mask_bits(prefix);
        Ok(Self { network: Ipv4Addr::from(u32::from(addr) & m), prefix })
    }
    fn mask_bits(prefix: u8) -> u32 {
        u32::MAX << (32 - prefix)
    }
    pub fn mask(&self) -> Ipv4Addr {
        Ipv4Addr::from(Self::mask_bits(self.prefix))
    }
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) | !Self::mask_bits(self.prefix))
    }
    /// Conventional gateway: the first host address.
    pub fn first_host(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }
    pub fn last_host(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast()) - 1)
    }
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & Self::mask_bits(self.prefix) == u32::from(self.network)
    }
    pub fn contains_host(&self, ip: Ipv4Addr) -> bool {
        self.contains(ip) && ip != self.network && ip != self.broadcast()
    }
}

impl FromStr for Cidr {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let (a, p) = s.trim().split_once('/').ok_or_else(|| anyhow!("`{s}`: expected a.b.c.d/n"))?;
        let addr: Ipv4Addr = a.parse().map_err(|e| anyhow!("`{s}`: {e}"))?;
        let prefix: u8 = p.parse().map_err(|e| anyhow!("`{s}`: {e}"))?;
        Self::new(addr, prefix)
    }
}
impl TryFrom<String> for Cidr {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}
impl From<Cidr> for String {
    fn from(c: Cidr) -> String {
        c.to_string()
    }
}
impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// `[network]` in `xdebbied.toml`. Presence turns on address allocation at
/// VM creation and (unless `dhcp = false`) the built-in DHCP responder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// The overlay subnet shared by every box's bridge.
    pub cidr: Cidr,
    /// Default route handed to VMs. Default: first host of `cidr`.
    #[serde(default)]
    pub gateway: Option<Ipv4Addr>,
    /// Resolvers handed to VMs. Default: the gateway.
    #[serde(default)]
    pub dns: Vec<Ipv4Addr>,
    /// Allocation pool. Defaults: `cidr.network + 16` ..= `cidr.last_host()`.
    /// Boxes that run independent control planes on one overlay must use
    /// disjoint pools.
    #[serde(default)]
    pub pool_start: Option<Ipv4Addr>,
    #[serde(default)]
    pub pool_end: Option<Ipv4Addr>,
    /// Never allocate these (appliances pinned by hand...).
    #[serde(default)]
    pub reserved: Vec<Ipv4Addr>,
    /// Where this box forwards its VMs' DNS: queries to the gateway on
    /// port 53 are DNAT'd to the first of these (bootstrap/firewall.rs).
    /// The first box takes its own resolvers; joiners copy the cluster's,
    /// so every VM asks the same server.
    #[serde(default)]
    pub upstream_dns: Vec<Ipv4Addr>,
    /// Run the DHCP responder on `bridge`. Off = allocate only.
    #[serde(default = "d_true")]
    pub dhcp: bool,
    /// Bridge to listen on. Default: the mesh bridge, else the first
    /// configured host's bridge.
    #[serde(default)]
    pub bridge: Option<String>,
    /// Lease time offered. Renewals are no-ops, so long is fine.
    #[serde(default = "d_lease")]
    pub lease_secs: u32,
    /// Option 15 (domain name), if set.
    #[serde(default)]
    pub domain: Option<String>,
    /// Answer only while this process is the elected control plane. Leave
    /// false while control planes keep independent stores (each box then
    /// answers for its own VMs and nobody else's).
    #[serde(default)]
    pub leader_only: bool,
}

fn d_true() -> bool {
    true
}
fn d_lease() -> u32 {
    DEFAULT_LEASE_SECS
}

impl NetworkConfig {
    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway.unwrap_or_else(|| self.cidr.first_host())
    }
    pub fn dns(&self) -> Vec<Ipv4Addr> {
        if self.dns.is_empty() { vec![self.gateway()] } else { self.dns.clone() }
    }
    pub fn pool(&self) -> (Ipv4Addr, Ipv4Addr) {
        let start = self
            .pool_start
            .unwrap_or_else(|| Ipv4Addr::from(u32::from(self.cidr.network) + POOL_SKIP));
        let end = self.pool_end.unwrap_or_else(|| self.cidr.last_host());
        (start, end)
    }

    pub fn validate(&self) -> Result<()> {
        let gw = self.gateway();
        if !self.cidr.contains_host(gw) {
            bail!("network.gateway {gw} is not a host address in {}", self.cidr);
        }
        let (start, end) = self.pool();
        if !self.cidr.contains_host(start) || !self.cidr.contains_host(end) {
            bail!("network pool {start}..{end} is not inside {}", self.cidr);
        }
        if u32::from(start) > u32::from(end) {
            bail!("network.pool_start {start} is after pool_end {end}");
        }
        if self.lease_secs < 60 {
            bail!("network.lease_secs must be >= 60");
        }
        if let Some(d) = &self.domain
            && (d.is_empty() || d.len() > 253)
        {
            bail!("network.domain must be 1..=253 chars");
        }
        Ok(())
    }

    /// Everything that may never be handed out, regardless of pool bounds.
    fn blocked(&self) -> BTreeSet<Ipv4Addr> {
        let mut b: BTreeSet<Ipv4Addr> = self.reserved.iter().copied().collect();
        b.insert(self.gateway());
        b.extend(self.dns().into_iter().filter(|d| self.cidr.contains(*d)));
        b
    }

    /// Addresses currently bound to VM records (every phase: a `Deleting` VM
    /// keeps its address until the record is gone).
    pub fn in_use<'a>(vms: impl Iterator<Item = &'a Vm>) -> BTreeSet<Ipv4Addr> {
        vms.filter_map(|v| v.ip).collect()
    }

    /// Pick the address for a new VM. `requested` must be a free host address
    /// in the subnet (it may sit outside the pool — that is how operators pin
    /// infrastructure addresses). Otherwise the lowest free pool address.
    pub fn allocate(&self, requested: Option<Ipv4Addr>, in_use: &BTreeSet<Ipv4Addr>) -> Result<Ipv4Addr> {
        let blocked = self.blocked();
        if let Some(r) = requested {
            if !self.cidr.contains_host(r) {
                bail!("requested ip {r} is not a host address in {}", self.cidr);
            }
            if blocked.contains(&r) {
                bail!("requested ip {r} is reserved");
            }
            if in_use.contains(&r) {
                bail!("requested ip {r} is already bound to another vm");
            }
            return Ok(r);
        }
        let (start, end) = self.pool();
        let mut ip = u32::from(start);
        while ip <= u32::from(end) {
            let cand = Ipv4Addr::from(ip);
            if !blocked.contains(&cand) && !in_use.contains(&cand) {
                return Ok(cand);
            }
            ip += 1;
        }
        bail!("network pool {start}..{end} is exhausted")
    }

    /// Give every VM created before `[network]` existed an address. Prefers an
    /// `InternalIP` the VM already reported (so a guest that holds a lease
    /// from a previous DHCP server keeps it), otherwise allocates.
    /// Returns `(vm id, ip)` pairs that were assigned.
    pub fn adopt(&self, vms: &mut std::collections::BTreeMap<Uuid, Vm>) -> Vec<(Uuid, Ipv4Addr)> {
        let mut in_use = Self::in_use(vms.values());
        let blocked = self.blocked();
        let mut out = Vec::new();
        let ids: Vec<Uuid> = vms.iter().filter(|(_, v)| v.ip.is_none()).map(|(id, _)| *id).collect();
        for id in ids {
            let vm = &vms[&id];
            let known = vm
                .addresses
                .iter()
                .filter(|a| a.kind == "InternalIP")
                .filter_map(|a| a.address.parse::<Ipv4Addr>().ok())
                .find(|ip| self.cidr.contains_host(*ip) && !blocked.contains(ip) && !in_use.contains(ip));
            let ip = match known {
                Some(ip) => ip,
                None => match self.allocate(None, &in_use) {
                    Ok(ip) => ip,
                    Err(_) => continue,
                },
            };
            in_use.insert(ip);
            vms.get_mut(&id).unwrap().ip = Some(ip);
            out.push((id, ip));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(cidr: &str) -> NetworkConfig {
        NetworkConfig {
            cidr: cidr.parse().unwrap(),
            gateway: None,
            dns: vec![],
            pool_start: None,
            pool_end: None,
            reserved: vec![],
            upstream_dns: vec![],
            dhcp: true,
            bridge: None,
            lease_secs: DEFAULT_LEASE_SECS,
            domain: None,
            leader_only: false,
        }
    }

    #[test]
    fn cidr_math() {
        let c: Cidr = "10.23.0.77/16".parse().unwrap();
        assert_eq!(c.network, Ipv4Addr::new(10, 23, 0, 0));
        assert_eq!(c.mask(), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(c.broadcast(), Ipv4Addr::new(10, 23, 255, 255));
        assert_eq!(c.first_host(), Ipv4Addr::new(10, 23, 0, 1));
        assert_eq!(c.last_host(), Ipv4Addr::new(10, 23, 255, 254));
        assert!(c.contains_host(Ipv4Addr::new(10, 23, 9, 9)));
        assert!(!c.contains_host(Ipv4Addr::new(10, 23, 255, 255)));
        assert!(!c.contains(Ipv4Addr::new(10, 24, 0, 1)));
        assert_eq!(c.to_string(), "10.23.0.0/16");
        assert!("10.0.0.0/31".parse::<Cidr>().is_err());
        assert!("10.0.0.0".parse::<Cidr>().is_err());
    }

    #[test]
    fn defaults_and_validation() {
        let n = cfg("10.23.0.0/16");
        assert_eq!(n.gateway(), Ipv4Addr::new(10, 23, 0, 1));
        assert_eq!(n.dns(), vec![Ipv4Addr::new(10, 23, 0, 1)]);
        assert_eq!(n.pool(), (Ipv4Addr::new(10, 23, 0, 16), Ipv4Addr::new(10, 23, 255, 254)));
        n.validate().unwrap();
        let mut bad = cfg("10.23.0.0/16");
        bad.gateway = Some(Ipv4Addr::new(10, 24, 0, 1));
        assert!(bad.validate().unwrap_err().to_string().contains("gateway"));
        let mut bad = cfg("10.23.0.0/16");
        bad.pool_start = Some(Ipv4Addr::new(10, 23, 1, 0));
        bad.pool_end = Some(Ipv4Addr::new(10, 23, 0, 200));
        assert!(bad.validate().unwrap_err().to_string().contains("pool_start"));
    }

    #[test]
    fn allocates_lowest_free_and_honours_requests() {
        let mut n = cfg("10.23.0.0/24");
        n.reserved = vec![Ipv4Addr::new(10, 23, 0, 16)];
        let mut used = BTreeSet::new();
        assert_eq!(n.allocate(None, &used).unwrap(), Ipv4Addr::new(10, 23, 0, 17));
        used.insert(Ipv4Addr::new(10, 23, 0, 17));
        assert_eq!(n.allocate(None, &used).unwrap(), Ipv4Addr::new(10, 23, 0, 18));
        // pinned outside the pool is fine
        assert_eq!(n.allocate(Some(Ipv4Addr::new(10, 23, 0, 5)), &used).unwrap(), Ipv4Addr::new(10, 23, 0, 5));
        let e = n.allocate(Some(Ipv4Addr::new(10, 23, 0, 17)), &used).unwrap_err().to_string();
        assert!(e.contains("already bound"), "{e}");
        let e = n.allocate(Some(Ipv4Addr::new(10, 23, 0, 1)), &used).unwrap_err().to_string();
        assert!(e.contains("reserved"), "{e}");
        let e = n.allocate(Some(Ipv4Addr::new(10, 99, 0, 1)), &used).unwrap_err().to_string();
        assert!(e.contains("not a host address"), "{e}");
    }

    fn vm(name: &str, ip: Option<Ipv4Addr>, reported: Option<&str>) -> Vm {
        use xdebbie_core::model::*;
        let now = chrono::Utc::now();
        Vm {
            id: Uuid::new_v4(),
            spec: VmSpec {
                name: name.into(),
                vcpus: 1,
                memory_mib: 512,
                disk_gib: 5,
                image: "img".into(),
                data_disk_gib: None,
                ip: None,
                storage: Default::default(),
                placement: Placement::default(),
                user_data: None,
                meta_data: Default::default(),
                labels: Default::default(),
            },
            phase: VmPhase::Running,
            power: PowerIntent::On,
            host: Some("h".into()),
            addresses: reported
                .map(|a| vec![Address { kind: "InternalIP".into(), address: a.into() }])
                .unwrap_or_default(),
            ip,
            disks: Vec::new(),
            message: None,
            migrate_to: None,
            reboot_generation: 0,
            observed_reboot_generation: 0,
            created: now,
            updated: now,
        }
    }

    #[test]
    fn adopt_keeps_reported_addresses_and_allocates_the_rest() {
        let n = cfg("10.23.0.0/16");
        let keep = vm("has-lease", None, Some("10.23.0.142"));
        let foreign = vm("foreign-lease", None, Some("192.168.100.5"));
        let fresh = vm("fresh", None, None);
        let done = vm("done", Some(Ipv4Addr::new(10, 23, 0, 16)), None);
        let mut vms: std::collections::BTreeMap<Uuid, Vm> =
            [keep.clone(), foreign.clone(), fresh.clone(), done.clone()].into_iter().map(|v| (v.id, v)).collect();
        let out = n.adopt(&mut vms);
        assert_eq!(out.len(), 3);
        assert_eq!(vms[&keep.id].ip, Some(Ipv4Addr::new(10, 23, 0, 142)));
        assert_eq!(vms[&done.id].ip, Some(Ipv4Addr::new(10, 23, 0, 16)));
        // the two allocations are .17 and .18 (.16 is taken, .142 is kept)
        let mut alloc: Vec<_> = [vms[&foreign.id].ip.unwrap(), vms[&fresh.id].ip.unwrap()].to_vec();
        alloc.sort();
        assert_eq!(alloc, vec![Ipv4Addr::new(10, 23, 0, 17), Ipv4Addr::new(10, 23, 0, 18)]);
        // idempotent
        assert!(n.adopt(&mut vms).is_empty());
    }

    #[test]
    fn pool_exhaustion() {
        let mut n = cfg("10.23.0.0/24");
        n.pool_start = Some(Ipv4Addr::new(10, 23, 0, 20));
        n.pool_end = Some(Ipv4Addr::new(10, 23, 0, 21));
        let mut used = BTreeSet::new();
        used.insert(n.allocate(None, &used).unwrap());
        used.insert(n.allocate(None, &used).unwrap());
        assert!(n.allocate(None, &used).unwrap_err().to_string().contains("exhausted"));
    }
}
