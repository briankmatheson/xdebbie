// SPDX-License-Identifier: GPL-3.0-or-later
use std::{net::SocketAddr, path::PathBuf};

use xdebbie_core::{Host, PlacementPolicy};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    #[serde(default = "d_listen")]
    pub listen: SocketAddr,
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
    /// `{meta}`, `{net}` are substituted.
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
}

fn d_discovery() -> bool {
    true
}
fn d_multicast() -> String {
    "239.0.0.42:7442".to_string()
}

fn d_listen() -> SocketAddr {
    "127.0.0.1:7443".parse().unwrap()
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
        "{out}",
        "{user}",
        "{meta}",
    ]
    .map(String::from)
    .to_vec()
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }
}
