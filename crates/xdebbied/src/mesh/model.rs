// SPDX-License-Identifier: GPL-3.0-or-later
//! Mesh data model: the desired state (`MeshSpec`), what we report about it
//! (`MeshStatus`), and the `[mesh]` TOML section that seeds the spec.
//! These types are also the API JSON.

use std::{collections::BTreeSet, net::Ipv4Addr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Geneve VNI used when none is configured.
pub const DEFAULT_VNI: u32 = 23;
/// IANA Geneve UDP port.
pub const DEFAULT_DSTPORT: u16 = 6081;
/// Outer IPv4 (20) + UDP (8) + Geneve base header (8) + inner Ethernet (14).
pub const GENEVE_OVERHEAD: u16 = 50;
/// Lowest MTU we accept (IPv6 minimum).
pub const MIN_MTU: u16 = 1280;
/// ifalias stamped on every link the mesh creates. Ownership = name + alias.
pub const OWNER_ALIAS: &str = "xdebbie-mesh/v1";
/// nftables table the mesh owns.
pub const NFT_TABLE: &str = "xdebbie_mesh";
/// Default WireGuard listen port.
pub const DEFAULT_WG_PORT: u16 = 51820;
/// WireGuard MTU on a 1500-byte carrier (IPv4 20 + UDP 8 + WG 32 = 60; 1420 is the wg-quick default).
pub const DEFAULT_WG_MTU: u16 = 1420;
/// Private key file inside the state directory.
pub const WG_KEY_FILE: &str = "wg.key";
/// Mesh networks blackholed when `blackholes` is absent (docs/mesh.md,
/// "Blackhole routes for the mesh networks"): the WireGuard underlay, the
/// VM overlay and the planned storage mesh.
pub const DEFAULT_BLACKHOLES: [&str; 3] = ["10.22.0.0/16", "10.23.0.0/16", "10.24.0.0/16"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FdbMode {
    #[default]
    Derived,
    Learn,
}

/// `"auto"` or a fixed number. Serialised as a string or an integer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MtuSpec {
    #[default]
    Auto,
    Fixed(u16),
}

impl Serialize for MtuSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MtuSpec::Auto => s.serialize_str("auto"),
            MtuSpec::Fixed(n) => s.serialize_u16(*n),
        }
    }
}

impl<'de> Deserialize<'de> for MtuSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(u64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Num(n) => u16::try_from(n)
                .map(MtuSpec::Fixed)
                .map_err(|_| serde::de::Error::custom("mtu out of range")),
            Raw::Str(s) if s.eq_ignore_ascii_case("auto") => Ok(MtuSpec::Auto),
            Raw::Str(s) => s.parse::<u16>().map(MtuSpec::Fixed).map_err(|_| {
                serde::de::Error::custom(format!("mtu must be \"auto\" or an integer, got {s:?}"))
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSpec {
    #[serde(default = "d_probe_interval")]
    pub interval_ms: u64,
    #[serde(default = "d_probe_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "d_three")]
    pub down_after: u32,
    #[serde(default = "d_three")]
    pub up_after: u32,
}

impl Default for ProbeSpec {
    fn default() -> Self {
        Self {
            interval_ms: d_probe_interval(),
            timeout_ms: d_probe_timeout(),
            down_after: d_three(),
            up_after: d_three(),
        }
    }
}

fn d_probe_interval() -> u64 {
    1000
}
fn d_probe_timeout() -> u64 {
    800
}
fn d_three() -> u32 {
    3
}
fn d_true() -> bool {
    true
}
fn d_vni() -> u32 {
    DEFAULT_VNI
}
fn d_dstport() -> u16 {
    DEFAULT_DSTPORT
}
fn d_bridge() -> String {
    "br-int".into()
}
fn d_underlay() -> String {
    "wg-flock".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    pub bridge: String,
    #[serde(default = "d_vni")]
    pub vni: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSpec {
    #[serde(rename = "box")]
    pub box_id: u8,
    /// Underlay address of the peer. Optional when the mesh manages
    /// WireGuard: `normalize` fills in `10.22.<box>.1`.
    #[serde(default = "unspecified")]
    pub remote: Ipv4Addr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Ipv4Addr>,
    /// WireGuard public key (base64). Required when `wireguard` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    /// WireGuard endpoint `host:port`. Optional: a peer without one is
    /// reached only after it initiates (NAT'd boxes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// A mesh client (xdj): a laptop or server that joins with one WireGuard
/// link to this box and routes the overlay through it. WireGuard peer only:
/// `allowed-ips = 10.22.<box>.1/32`, no Geneve tunnel, no probe, no FDB, and
/// never part of Ready/Degraded (a client being away is normal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSpec {
    #[serde(rename = "box")]
    pub box_id: u8,
    /// WireGuard public key (base64), from `xdj` on the client.
    pub pubkey: String,
    /// Free-form label (`laptop`, `gram`) for `xdctl mesh ls` / `client rm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// WireGuard endpoint `host:port`. Usually absent: clients initiate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl ClientSpec {
    /// The client's address on the underlay: `10.22.<box>.1`.
    pub fn addr(&self) -> Ipv4Addr {
        underlay_addr(self.box_id)
    }
}

fn unspecified() -> Ipv4Addr {
    Ipv4Addr::UNSPECIFIED
}

/// Underlay address of a box when the mesh manages WireGuard: `10.22.<box>.1`.
pub fn underlay_addr(box_id: u8) -> Ipv4Addr {
    Ipv4Addr::new(10, 22, box_id, 1)
}

/// A box's address on the shared overlay bridge: `10.23.<box>.1`.
pub fn bridge_addr(box_id: u8) -> Ipv4Addr {
    Ipv4Addr::new(10, 23, box_id, 1)
}

/// `[mesh.wireguard]` / `spec.wireguard`: when present the mesh owns the
/// underlay interface named by `spec.underlay` and keeps its peers in sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireguardSpec {
    #[serde(default = "d_wg_port")]
    pub listen_port: u16,
    /// Address on the interface. Default `10.22.<box>.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
    #[serde(default = "d_wg_prefix")]
    pub prefix: u8,
    #[serde(default = "d_wg_mtu")]
    pub mtu: u16,
    /// `persistent-keepalive` seconds for every peer (0 = off).
    #[serde(default = "d_wg_keepalive")]
    pub keepalive: u16,
}

impl Default for WireguardSpec {
    fn default() -> Self {
        Self {
            listen_port: d_wg_port(),
            address: None,
            prefix: d_wg_prefix(),
            mtu: d_wg_mtu(),
            keepalive: d_wg_keepalive(),
        }
    }
}

fn d_wg_port() -> u16 {
    DEFAULT_WG_PORT
}
fn d_wg_prefix() -> u8 {
    16
}
fn d_wg_mtu() -> u16 {
    DEFAULT_WG_MTU
}
fn d_wg_keepalive() -> u16 {
    25
}

/// Desired state. `PUT /v1/mesh` replaces the whole thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshSpec {
    /// Default true: a box meshes unless told not to.
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(rename = "box", default)]
    pub box_id: u8,
    /// v1: exactly one entry.
    #[serde(default)]
    pub networks: Vec<NetworkSpec>,
    #[serde(default = "d_dstport")]
    pub dstport: u16,
    #[serde(default = "d_underlay")]
    pub underlay: String,
    #[serde(default)]
    pub allow_plain_underlay: bool,
    #[serde(default)]
    pub mtu: MtuSpec,
    #[serde(default)]
    pub fdb: FdbMode,
    #[serde(default = "d_true")]
    pub firewall: bool,
    #[serde(default)]
    pub probe: ProbeSpec,
    /// Manage the WireGuard underlay too (0.5.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardSpec>,
    #[serde(default)]
    pub peers: Vec<PeerSpec>,
    /// xdj clients: WireGuard-only peers (needs `wireguard`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<ClientSpec>,
    /// CIDRs to blackhole at the worst metric so a vanished connected
    /// route drops traffic instead of leaking it (docs/mesh.md, "Blackhole
    /// routes for the mesh networks"). `[]` turns this off.
    #[serde(default = "d_blackholes")]
    pub blackholes: Vec<String>,
}

fn d_blackholes() -> Vec<String> {
    DEFAULT_BLACKHOLES.iter().map(|s| (*s).to_owned()).collect()
}

/// `a.b.c.d/n` with 1 <= n <= 32 and no host bits set; `None` otherwise.
/// A /0 is refused: blackholing everything is never what a mesh wants.
pub fn parse_net(cidr: &str) -> Option<(Ipv4Addr, u8)> {
    let (addr, prefix) = cidr.split_once('/')?;
    let addr: Ipv4Addr = addr.parse().ok()?;
    let prefix: u8 = prefix.parse().ok().filter(|p| (1..=32).contains(p))?;
    (u32::from(addr) & !prefix_mask(prefix) == 0).then_some((addr, prefix))
}

/// Netmask for a prefix length, as a host-order `u32`.
pub fn prefix_mask(prefix: u8) -> u32 {
    match prefix {
        0 => 0,
        p => u32::MAX << (32 - u32::from(p.min(32))),
    }
}

/// Whether `ip` lies inside `net`/`prefix`.
pub fn net_contains((net, prefix): (Ipv4Addr, u8), ip: Ipv4Addr) -> bool {
    u32::from(ip) & prefix_mask(prefix) == u32::from(net)
}

impl Default for MeshSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            box_id: 0,
            networks: vec![NetworkSpec {
                bridge: d_bridge(),
                vni: d_vni(),
            }],
            dstport: d_dstport(),
            underlay: d_underlay(),
            allow_plain_underlay: false,
            mtu: MtuSpec::Auto,
            fdb: FdbMode::Derived,
            firewall: true,
            probe: ProbeSpec::default(),
            wireguard: None,
            peers: vec![],
            clients: vec![],
            blackholes: d_blackholes(),
        }
    }
}

impl MeshSpec {
    /// The blackhole routes this spec wants (docs/mesh.md, "Blackhole
    /// routes for the mesh networks"): every valid `blackholes` entry.
    /// Without managed WireGuard the underlay is the operator's, so a net
    /// holding a peer's `remote` is skipped: our blackhole could beat the
    /// operator's route to that peer (a default or gateway route).
    pub fn blackhole_nets(&self) -> BTreeSet<(Ipv4Addr, u8)> {
        let underlay_ours = self.wireguard.is_some();
        self.blackholes
            .iter()
            .filter_map(|c| parse_net(c))
            .filter(|&n| underlay_ours || !self.peers.iter().any(|p| net_contains(n, p.remote)))
            .collect()
    }
    /// Fill derivable fields: with managed WireGuard a peer's `remote`
    /// defaults to `10.22.<box>.1`. Run before validation.
    pub fn normalize(mut self) -> Self {
        if self.wireguard.is_some() {
            for p in &mut self.peers {
                if p.remote.is_unspecified() {
                    p.remote = underlay_addr(p.box_id);
                }
            }
        }
        self
    }
    /// Local WireGuard address (`wireguard.address` or `10.22.<box>.1`).
    pub fn wg_address(&self) -> Option<Ipv4Addr> {
        self.wireguard
            .as_ref()
            .map(|w| w.address.unwrap_or_else(|| underlay_addr(self.box_id)))
    }
    /// The single v1 network. Validation guarantees it exists; callers that
    /// run before validation get the default.
    pub fn network(&self) -> NetworkSpec {
        self.networks.first().cloned().unwrap_or(NetworkSpec {
            bridge: d_bridge(),
            vni: d_vni(),
        })
    }
    pub fn bridge(&self) -> String {
        self.network().bridge
    }
    pub fn vni(&self) -> u32 {
        self.network().vni
    }
    /// Effective probe address for a peer (section 3.2).
    pub fn probe_addr(&self, p: &PeerSpec) -> Ipv4Addr {
        match (p.probe, &self.fdb) {
            (Some(a), _) => a,
            (None, FdbMode::Derived) => bridge_addr(p.box_id),
            (None, FdbMode::Learn) => p.remote,
        }
    }
}

#[cfg(test)]
impl PeerSpec {
    pub fn new(box_id: u8, remote: Ipv4Addr) -> Self {
        Self {
            box_id,
            remote,
            probe: None,
            pubkey: None,
            endpoint: None,
        }
    }
}

#[cfg(test)]
impl MeshStatus {
    pub fn condition(&self, kind: ConditionType) -> Option<&Condition> {
        self.conditions.iter().find(|c| c.kind == kind)
    }
    pub fn is(&self, kind: ConditionType) -> bool {
        self.condition(kind).map(|c| c.status).unwrap_or(false)
    }
}

/// Tunnel device name for a peer: `gnv-b<P>`, decimal, no padding.
pub fn tunnel_name(box_id: u8) -> String {
    format!("gnv-b{box_id}")
}

/// `gnv-b<P>` → `P`, if the name has the mesh shape.
pub fn parse_tunnel_name(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("gnv-b")?;
    if rest.is_empty() || rest.len() > 3 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // No leading zeros: "gnv-b01" is not ours.
    if rest.len() > 1 && rest.starts_with('0') {
        return None;
    }
    rest.parse::<u8>().ok().filter(|b| *b <= 253)
}

/// Host bridge MAC for a box: `52:54:00:23:<box hex>:fe`.
pub fn bridge_mac(box_id: u8) -> String {
    format!("52:54:00:23:{box_id:02x}:fe")
}

/// Slot MAC for a box and slot (0..16): `52:54:00:23:<box hex>:<slot hex>`.
pub fn slot_mac(box_id: u8, slot: u8) -> String {
    format!("52:54:00:23:{box_id:02x}:{slot:02x}")
}

/// The 17 MACs the `derived` plan assigns to a box.
pub fn derived_macs(box_id: u8) -> Vec<String> {
    let mut v: Vec<String> = (0u8..16).map(|s| slot_mac(box_id, s)).collect();
    v.push(bridge_mac(box_id));
    v
}

/// `mesh_mtu = mtu(underlay) − 50 − option_bytes` (option_bytes = 0 in v1).
pub fn auto_mtu(underlay_mtu: u32) -> u16 {
    underlay_mtu
        .saturating_sub(GENEVE_OVERHEAD as u32)
        .min(u16::MAX as u32) as u16
}

// ---------- status ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Api,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum PeerState {
    Pending,
    Up,
    Down,
    Error,
}

impl PeerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerState::Pending => "Pending",
            PeerState::Up => "Up",
            PeerState::Down => "Down",
            PeerState::Error => "Error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerStatus {
    #[serde(rename = "box")]
    pub box_id: u8,
    pub remote: Ipv4Addr,
    pub probe: Ipv4Addr,
    pub device: String,
    pub ifindex: Option<u32>,
    pub state: PeerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<f64>,
    pub last_change: DateTime<Utc>,
    pub fdb_entries: u32,
    /// Interface the kernel would route `remote` through. Reported for
    /// debugging when `allow_plain_underlay` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_dev: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum ConditionType {
    Ready,
    Degraded,
    GeneveModuleMissing,
    UnderlayMissing,
    BridgeMissing,
    BridgeStpEnabled,
    BridgeMacMismatch,
    /// The bridge holds an overlay address of another box (10.23.<other>.x):
    /// it would answer for that box on the shared overlay. Blocking.
    BridgeAddressMismatch,
    NameConflict,
    /// `/sys/module/wireguard` missing while `wireguard` is configured.
    WireguardModuleMissing,
    /// The underlay name exists but is not a WireGuard link we own.
    WireguardConflict,
}

impl ConditionType {
    /// Blocking conditions stop reconcile actions (but never tear down).
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            ConditionType::GeneveModuleMissing
                | ConditionType::UnderlayMissing
                | ConditionType::BridgeMissing
                | ConditionType::BridgeAddressMismatch
                | ConditionType::WireguardModuleMissing
                | ConditionType::WireguardConflict
        )
    }
    pub const ALL: [ConditionType; 11] = [
        ConditionType::GeneveModuleMissing,
        ConditionType::WireguardModuleMissing,
        ConditionType::WireguardConflict,
        ConditionType::UnderlayMissing,
        ConditionType::BridgeMissing,
        ConditionType::BridgeStpEnabled,
        ConditionType::BridgeMacMismatch,
        ConditionType::BridgeAddressMismatch,
        ConditionType::NameConflict,
        ConditionType::Degraded,
        ConditionType::Ready,
    ];
}

/// Per-peer WireGuard view (from `wg show <dev> dump`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WgPeerStatus {
    #[serde(rename = "box")]
    pub box_id: u8,
    pub pubkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Seconds since the last handshake; `None` = never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handshake_age_s: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// An xdj client (`spec.clients`), not a Geneve peer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub client: bool,
    /// The client's `name`, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireguardStatus {
    pub device: String,
    /// This box's public key — what the other boxes put in their peer entry.
    pub pubkey: String,
    pub listen_port: u16,
    pub address: String,
    pub peers: Vec<WgPeerStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    #[serde(rename = "type")]
    pub kind: ConditionType,
    pub status: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub last_transition: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MeshStatus {
    pub source: Option<Source>,
    pub generation: u64,
    pub observed_generation: u64,
    pub mtu: u16,
    pub conditions: Vec<Condition>,
    pub peers: Vec<PeerStatus>,
    pub vms_mtu_mismatch: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardStatus>,
    #[serde(default)]
    pub metrics: super::metrics::Snapshot,
}

// ---------- TOML `[mesh]` ----------

/// `[mesh]` in xdebbied.toml. Flat shape (bridge/vni at top level) as in
/// the spec; converted to a `MeshSpec` with `to_spec`.
///
/// The mesh is on by default: a config with no `[mesh]` section at all is
/// this struct's `Default` with `bridge` resolved to the first host's bridge
/// (`Config::mesh_config`). Unless `allow_plain_underlay = true` the
/// WireGuard underlay is managed too, with `[mesh.wireguard]` defaults.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(rename = "box", default)]
    pub box_id: u8,
    /// Overlay bridge. Default: the first `[[hosts]]` bridge (see
    /// `Config::mesh_config`), else `br-int`.
    #[serde(default)]
    pub bridge: Option<String>,
    #[serde(default = "d_vni")]
    pub vni: u32,
    #[serde(default = "d_dstport")]
    pub dstport: u16,
    #[serde(default = "d_underlay")]
    pub underlay: String,
    #[serde(default)]
    pub allow_plain_underlay: bool,
    #[serde(default)]
    pub mtu: MtuSpec,
    #[serde(default)]
    pub fdb: FdbMode,
    #[serde(default = "d_true")]
    pub firewall: bool,
    #[serde(default = "d_probe_interval")]
    pub probe_interval_ms: u64,
    #[serde(default = "d_probe_timeout")]
    pub probe_timeout_ms: u64,
    #[serde(default = "d_three")]
    pub probe_down_after: u32,
    #[serde(default = "d_three")]
    pub probe_up_after: u32,
    /// `[mesh.wireguard]`
    #[serde(default)]
    pub wireguard: Option<WireguardSpec>,
    #[serde(default)]
    pub peers: Vec<PeerSpec>,
    /// `[[mesh.clients]]`
    #[serde(default)]
    pub clients: Vec<ClientSpec>,
    /// Mesh networks to blackhole (docs/mesh.md, "Blackhole routes for the
    /// mesh networks"). Absent = `DEFAULT_BLACKHOLES`; `[]` = none.
    #[serde(default = "d_blackholes")]
    pub blackholes: Vec<String>,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            box_id: 0,
            bridge: None,
            vni: d_vni(),
            dstport: d_dstport(),
            underlay: d_underlay(),
            allow_plain_underlay: false,
            mtu: MtuSpec::Auto,
            fdb: FdbMode::Derived,
            firewall: true,
            probe_interval_ms: d_probe_interval(),
            probe_timeout_ms: d_probe_timeout(),
            probe_down_after: d_three(),
            probe_up_after: d_three(),
            wireguard: None,
            peers: vec![],
            clients: vec![],
            blackholes: d_blackholes(),
        }
    }
}

impl MeshConfig {
    /// The overlay bridge this section names, or the built-in fallback.
    pub fn bridge(&self) -> String {
        self.bridge.clone().unwrap_or_else(d_bridge)
    }

    pub fn to_spec(&self) -> MeshSpec {
        MeshSpec {
            enabled: self.enabled,
            box_id: self.box_id,
            networks: vec![NetworkSpec {
                bridge: self.bridge(),
                vni: self.vni,
            }],
            dstport: self.dstport,
            underlay: self.underlay.clone(),
            allow_plain_underlay: self.allow_plain_underlay,
            mtu: self.mtu.clone(),
            fdb: self.fdb.clone(),
            firewall: self.firewall,
            probe: ProbeSpec {
                interval_ms: self.probe_interval_ms,
                timeout_ms: self.probe_timeout_ms,
                down_after: self.probe_down_after,
                up_after: self.probe_up_after,
            },
            // Managed WireGuard underlay unless the operator brings their own.
            wireguard: self
                .wireguard
                .clone()
                .or_else(|| (!self.allow_plain_underlay).then(WireguardSpec::default)),
            peers: self.peers.clone(),
            clients: self.clients.clone(),
            blackholes: self.blackholes.clone(),
        }
        .normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_names() {
        assert_eq!(tunnel_name(0), "gnv-b0");
        assert_eq!(tunnel_name(17), "gnv-b17");
        assert_eq!(tunnel_name(253), "gnv-b253");
        assert_eq!(parse_tunnel_name("gnv-b0"), Some(0));
        assert_eq!(parse_tunnel_name("gnv-b253"), Some(253));
        assert_eq!(parse_tunnel_name("gnv-b254"), None);
        assert_eq!(parse_tunnel_name("gnv-b01"), None);
        assert_eq!(parse_tunnel_name("gnv-b"), None);
        assert_eq!(parse_tunnel_name("gnv-b1x"), None);
        assert_eq!(parse_tunnel_name("vnet3"), None);
    }

    #[test]
    fn macs() {
        assert_eq!(bridge_mac(1), "52:54:00:23:01:fe");
        assert_eq!(slot_mac(1, 5), "52:54:00:23:01:05");
        assert_eq!(bridge_mac(253), "52:54:00:23:fd:fe");
        assert_eq!(derived_macs(7).len(), 17);
    }

    #[test]
    fn mtu_math() {
        assert_eq!(auto_mtu(1420), 1370);
        assert_eq!(auto_mtu(8920), 8870);
        assert_eq!(auto_mtu(1500), 1450);
    }

    #[test]
    fn mtu_spec_serde() {
        let a: MtuSpec = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(a, MtuSpec::Auto);
        let f: MtuSpec = serde_json::from_str("1370").unwrap();
        assert_eq!(f, MtuSpec::Fixed(1370));
        let t: MtuSpec = toml::from_str::<toml::Value>("m = \"auto\"")
            .unwrap()
            .get("m")
            .cloned()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(t, MtuSpec::Auto);
        assert_eq!(serde_json::to_string(&MtuSpec::Auto).unwrap(), "\"auto\"");
        assert_eq!(
            serde_json::to_string(&MtuSpec::Fixed(9000)).unwrap(),
            "9000"
        );
    }

    #[test]
    fn toml_section_round_trip() {
        let c: MeshConfig = toml::from_str(
            r#"
enabled = true
box = 3
bridge = "virbr-int"
underlay = "ul0"
allow_plain_underlay = true
fdb = "learn"
[[peers]]
box = 1
remote = "10.22.1.1"
probe = "10.23.1.1"
"#,
        )
        .unwrap();
        let s = c.to_spec();
        assert!(s.enabled);
        assert_eq!(s.box_id, 3);
        assert_eq!(s.bridge(), "virbr-int");
        assert_eq!(s.vni(), 23);
        assert_eq!(s.fdb, FdbMode::Learn);
        assert_eq!(s.peers.len(), 1);
        assert_eq!(
            s.probe_addr(&s.peers[0]),
            "10.23.1.1".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn probe_defaults() {
        let mut s = MeshSpec::default();
        let p = PeerSpec::new(9, "10.22.9.1".parse().unwrap());
        assert_eq!(s.probe_addr(&p), "10.23.9.1".parse::<Ipv4Addr>().unwrap());
        s.fdb = FdbMode::Learn;
        assert_eq!(s.probe_addr(&p), p.remote);
    }

    #[test]
    fn wireguard_normalizes_remote_and_roundtrips() {
        let j = r#"{"enabled":true,"box":0,"networks":[{"bridge":"br-int"}],"wireguard":{},
                    "peers":[{"box":1,"pubkey":"k1"},{"box":2,"remote":"192.0.2.9","pubkey":"k2","endpoint":"gram:51820"}]}"#;
        let s: MeshSpec = serde_json::from_str(j).unwrap();
        assert!(s.peers[0].remote.is_unspecified());
        let s = s.normalize();
        assert_eq!(s.peers[0].remote, Ipv4Addr::new(10, 22, 1, 1));
        assert_eq!(s.peers[1].remote, Ipv4Addr::new(192, 0, 2, 9));
        let w = s.wireguard.as_ref().unwrap();
        assert_eq!(
            (w.listen_port, w.prefix, w.mtu, w.keepalive),
            (51820, 16, 1420, 25)
        );
        assert_eq!(s.wg_address(), Some(Ipv4Addr::new(10, 22, 0, 1)));
        let back: MeshSpec = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // without wireguard a missing remote stays unspecified (validation rejects it)
        let s2: MeshSpec = serde_json::from_str(r#"{"peers":[{"box":1}]}"#).unwrap();
        assert!(s2.normalize().peers[0].remote.is_unspecified());
        // TOML shape
        let t: MeshConfig = toml::from_str(
            "box = 3\n[wireguard]\nlisten_port = 51821\n[[peers]]\nbox = 1\npubkey = \"k\"\n",
        )
        .unwrap();
        let sp = t.to_spec();
        assert_eq!(sp.wireguard.unwrap().listen_port, 51821);
        assert_eq!(sp.peers[0].remote, Ipv4Addr::new(10, 22, 1, 1));
    }

    #[test]
    fn clients_serde() {
        // absent = empty, and an empty list is left out again
        let s: MeshSpec = serde_json::from_str(r#"{"wireguard":{}}"#).unwrap();
        assert!(s.clients.is_empty());
        assert!(serde_json::to_value(&s).unwrap().get("clients").is_none());
        let j = r#"{"wireguard":{},"clients":[{"box":3,"pubkey":"k","name":"laptop"}]}"#;
        let s: MeshSpec = serde_json::from_str(j).unwrap();
        assert_eq!(s.clients[0].addr(), Ipv4Addr::new(10, 22, 3, 1));
        assert_eq!(
            serde_json::to_value(&s).unwrap()["clients"],
            serde_json::json!([{"box": 3, "pubkey": "k", "name": "laptop"}])
        );
        // unknown fields and a missing pubkey are rejected
        assert!(
            serde_json::from_str::<MeshSpec>(
                r#"{"clients":[{"box":3,"pubkey":"k","probe":"1.2.3.4"}]}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<MeshSpec>(r#"{"clients":[{"box":3}]}"#).is_err());
        // TOML
        let c: MeshConfig =
            toml::from_str("[[clients]]\nbox = 4\npubkey = \"k\"\nname = \"gram\"\n").unwrap();
        assert_eq!(c.to_spec().clients[0].box_id, 4);
    }
}
