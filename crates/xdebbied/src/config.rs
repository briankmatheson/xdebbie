// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use xdebbie_core::{Host, PlacementPolicy};
use serde::Deserialize;
use tracing::warn;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    /// API listen address. Omit (or set to 0.0.0.0:7443) to auto-detect the
    /// IP of the interface that holds the default route. Explicit value always
    /// wins.
    #[serde(default)]
    pub listen: Option<SocketAddr>,
    #[serde(default = "d_state")]
    pub state_file: PathBuf,
    /// File holding the API bearer token. Unset = no auth (lab use only).
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// PEM certificate chain and key. Both set = HTTPS; neither = plain HTTP.
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    #[serde(default)]
    pub policy: PlacementPolicy,
    #[serde(default = "d_interval")]
    pub reconcile_interval_secs: u64,
    #[serde(default = "d_stale")]
    pub host_stale_secs: i64,
    /// Path to the `virsh` binary on the xdebbied machine.
    #[serde(default = "d_virsh")]
    pub virsh: String,
    /// Command used to build the NoCloud seed ISO. `{out}`, `{user}`,
    /// `{meta}`, `{net}`, `{vendor}` are substituted.
    #[serde(default = "d_seed")]
    pub seed_command: Vec<String>,
    /// Hosts to register at startup (merged with persisted state).
    #[serde(default)]
    pub hosts: Vec<Host>,
    /// Enable multicast discovery (default: true)
    #[serde(default = "d_discovery")]
    pub enable_discovery: bool,
    /// Multicast address for discovery
    #[serde(default = "d_multicast")]
    pub multicast_addr: String,
    /// LVM volume group for VM disks (optional, uses qcow2 if not set)
    #[serde(default)]
    pub volume_group: Option<String>,
    /// Thin pool name for thin provisioning (optional)
    #[serde(default)]
    pub thin_pool: Option<String>,
    /// Geneve mesh between hypervisors (`[mesh]`). Off unless `enabled = true`.
    #[serde(default)]
    pub mesh: Option<crate::mesh::MeshConfig>,
    /// Overlay addressing + built-in DHCP (`[network]`). Absent = VMs get no
    /// address from xdebbie and some other DHCP server must exist.
    #[serde(default)]
    pub network: Option<crate::dhcp::NetworkConfig>,
}

fn d_discovery() -> bool {
    true
}
fn d_multicast() -> String {
    "239.0.0.42:7442".to_string()
}
fn d_state() -> PathBuf {
    "/var/lib/xdebbie/state.json".into()
}
fn d_interval() -> u64 {
    5
}
fn d_stale() -> i64 {
    60
}
fn d_virsh() -> String {
    "virsh".into()
}
fn d_seed() -> Vec<String> {
    [
        "cloud-localds",
        "--network-config={net}",
        "--vendor-data={vendor}",
        "{out}",
        "{user}",
        "{meta}",
    ]
    .map(String::from)
    .to_vec()
}

/// Read `/proc/net/route` and return the IP address of the interface that
/// carries the default route (destination == 0.0.0.0, mask == 0.0.0.0).
/// Returns `None` if no default route is found or any parse step fails.
fn default_route_ip() -> Option<Ipv4Addr> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    // Find the default-route line: destination field (col 1) == "00000000"
    // and mask field (col 7) == "00000000".
    let iface = table.lines().skip(1).find_map(|line| {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?;
        let dest  = cols.next()?;
        // skip flags, refcnt, use, metric
        let _flags  = cols.next()?;
        let _refs   = cols.next()?;
        let _use    = cols.next()?;
        let _metric = cols.next()?;
        let mask    = cols.next()?;
        if dest == "00000000" && mask == "00000000" {
            Some(iface.to_string())
        } else {
            None
        }
    })?;

    // Walk the interface list via getifaddrs to find the IPv4 address
    // assigned to that interface.
    // SAFETY: standard getifaddrs usage; all pointers are checked before use.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut cursor = ifap;
        let mut found: Option<Ipv4Addr> = None;
        while !cursor.is_null() {
            let ifa = &*cursor;
            if !ifa.ifa_name.is_null() && !ifa.ifa_addr.is_null() {
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name)
                    .to_string_lossy();
                if name == iface {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    if (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                        let octets = sa.sin_addr.s_addr.to_ne_bytes();
                        found = Some(Ipv4Addr::from(octets));
                        break;
                    }
                }
            }
            cursor = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        found
    }
}

const DEFAULT_PORT: u16 = 7443;

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        if let Some(n) = &cfg.network {
            n.validate().map_err(|e| anyhow::anyhow!("[network]: {e}"))?;
        }
        Ok(cfg)
    }

    /// Bridge the DHCP responder listens on: explicit, else the mesh bridge,
    /// else the first configured host's bridge.
    pub fn dhcp_bridge(&self) -> Option<String> {
        let n = self.network.as_ref()?;
        n.bridge
            .clone()
            .or_else(|| self.mesh.as_ref().map(|m| m.bridge.clone()))
            .or_else(|| self.hosts.first().map(|h| h.bridge.clone()))
    }

    /// Resolve the effective listen address.
    ///
    /// Priority:
    ///   1. `listen` explicitly set in config — used as-is.
    ///   2. Auto-detected IP of the default-route interface, port 7443.
    ///   3. Fallback: `127.0.0.1:7443` (with a warning).
    pub fn effective_listen(&self) -> SocketAddr {
        // 1. Explicit config value.
        if let Some(addr) = self.listen {
            return addr;
        }

        // 2. Auto-detect from default route.
        if let Some(ip) = default_route_ip() {
            let addr = SocketAddr::new(IpAddr::V4(ip), DEFAULT_PORT);
            tracing::info!(
                "listen not configured — using default-route interface address {}",
                addr
            );
            return addr;
        }

        // 3. Last-resort fallback.
        warn!(
            "could not determine default-route IP; falling back to 127.0.0.1:{}. \
             Set `listen` in xdebbied.toml to fix peer discovery.",
            DEFAULT_PORT
        );
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
    }

    /// Directory holding `state.json`, `mesh.json`, ...
    pub fn state_dir(&self) -> PathBuf {
        self.state_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
