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
    /// URL peers and clients use to reach this box's API, e.g.
    /// `https://node0.example.net:7443`. Default: scheme + the listen
    /// address, with an unspecified IP (0.0.0.0) replaced by the
    /// default-route interface address.
    #[serde(default)]
    pub advertise_url: Option<String>,
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
    /// `nvme` and `qemu-img` binaries used for `storage = abe` disks, and the
    /// sysfs directory listing NVMe controllers (overridable for tests).
    #[serde(default = "d_nvme")]
    pub nvme: String,
    #[serde(default = "d_qemu_img")]
    pub qemu_img: String,
    #[serde(default = "d_nvme_sys")]
    pub nvme_sys: String,
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
    /// Geneve mesh between hypervisors (`[mesh]`). On by default; absent
    /// means "mesh on the first host's bridge with a managed WireGuard
    /// underlay" (`mesh_config`). Set `enabled = false` to opt out.
    #[serde(default)]
    pub mesh: Option<crate::mesh::MeshConfig>,
    /// Overlay addressing + built-in DHCP (`[network]`). Absent = VMs get no
    /// address from xdebbie and some other DHCP server must exist.
    #[serde(default)]
    pub network: Option<crate::dhcp::NetworkConfig>,
}

fn d_nvme() -> String {
    "nvme".into()
}
fn d_qemu_img() -> String {
    "qemu-img".into()
}
fn d_nvme_sys() -> String {
    "/sys/class/nvme".into()
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

pub const DEFAULT_PORT: u16 = 7443;

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        if let Some(n) = &cfg.network {
            n.validate().map_err(|e| anyhow::anyhow!("[network]: {e}"))?;
        }
        Ok(cfg)
    }

    /// The effective `[mesh]` section: what was written, with an unset
    /// `bridge` resolved to the first configured host's bridge; or, with no
    /// section at all, the defaults (enabled, managed WireGuard) on that
    /// same bridge.
    pub fn mesh_config(&self) -> crate::mesh::MeshConfig {
        let mut m = self.mesh.clone().unwrap_or_default();
        if m.bridge.is_none() {
            m.bridge = self.hosts.first().map(|h| h.bridge.clone());
        }
        m
    }

    /// Bridge the DHCP responder listens on: explicit, else the mesh bridge
    /// (which itself defaults to the first configured host's bridge).
    pub fn dhcp_bridge(&self) -> Option<String> {
        let n = self.network.as_ref()?;
        n.bridge.clone().or_else(|| Some(self.mesh_config().bridge()))
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

    /// Where other boxes and xdctl reach this API (announced in discovery
    /// and `GET /v1/leader`).
    pub fn advertise_url(&self, listen: SocketAddr) -> String {
        if let Some(u) = &self.advertise_url {
            return u.trim_end_matches('/').to_string();
        }
        let scheme = if self.tls_cert.is_some() { "https" } else { "http" };
        let ip = if listen.ip().is_unspecified() {
            default_route_ip().map(IpAddr::V4).unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
        } else {
            listen.ip()
        };
        format!("{scheme}://{}", SocketAddr::new(ip, listen.port()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_is_on_by_default_on_the_first_hosts_bridge() {
        let base = r#"
[[hosts]]
name = "a.example"
uri = "qemu:///system"
bridge = "virbr-xdebbie"
storage_pool = "boot"
"#;
        let cfg: Config = toml::from_str(base).unwrap();
        let m = cfg.mesh_config();
        let s = m.to_spec();
        assert!(s.enabled);
        assert_eq!(s.bridge(), "virbr-xdebbie");
        assert_eq!(s.underlay, "wg-flock");
        assert!(s.wireguard.is_some(), "WireGuard underlay is managed by default");
        assert_eq!(cfg.dhcp_bridge(), None, "no [network] => no responder");

        let cfg: Config = toml::from_str(&format!("{base}\n[mesh]\nenabled = false\n")).unwrap();
        assert!(!cfg.mesh_config().to_spec().enabled);

        // bringing your own underlay switches managed WireGuard off
        let cfg: Config = toml::from_str(&format!("{base}\n[mesh]\nunderlay = \"ul0\"\nallow_plain_underlay = true\n")).unwrap();
        let s = cfg.mesh_config().to_spec();
        assert!(s.enabled && s.wireguard.is_none());
        assert_eq!(s.bridge(), "virbr-xdebbie");
    }

    #[test]
    fn advertise_url_follows_listen_unless_set() {
        let c: Config = toml::from_str("state_file = 'x'").unwrap();
        assert_eq!(c.advertise_url("10.0.0.5:7443".parse().unwrap()), "http://10.0.0.5:7443");

        let c: Config = toml::from_str("state_file = 'x'\ntls_cert = 'c'\ntls_key = 'k'").unwrap();
        assert_eq!(c.advertise_url("10.0.0.5:7443".parse().unwrap()), "https://10.0.0.5:7443");

        let c: Config = toml::from_str("state_file = 'x'\nadvertise_url = 'https://node0.example.net:7443/'").unwrap();
        assert_eq!(c.advertise_url("0.0.0.0:7443".parse().unwrap()), "https://node0.example.net:7443");

        // 0.0.0.0 is never advertised.
        let c: Config = toml::from_str("state_file = 'x'").unwrap();
        assert!(!c.advertise_url("0.0.0.0:7443".parse().unwrap()).contains("0.0.0.0"));
    }
}
