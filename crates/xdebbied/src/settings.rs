// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings that change while the daemon runs: `GET`/`PUT /v1/settings`.
//!
//! | key | means | default |
//! |---|---|---|
//! | `mesh_routed` | routed mesh peers (xdj) may reach this box's VMs (`[network] mesh_routed`) | off |
//! | `discovery` | take part in LAN discovery and election (`enable_discovery`) | off |
//!
//! What was last set is kept in `settings.json` beside `state_file` and
//! overrides `xdebbied.toml` wherever the config is loaded
//! ([`apply_saved`]), so bootstrap's firewall pass and the next start agree
//! with it.
//!
//! A change is applied in place, never by re-running the firewall: that
//! flushes the `xdebbie_host` table, and with it rules other tools insert
//! there (aviary's `set-nat.sh`). Each knob adds or removes only its own
//! rules — the ufw rules that differ between the knob on and off, and for
//! `mesh_routed` the one nft accept tagged [`ROUTED_TAG`].
//!
//! Discovery off pauses the engine: it neither sends nor listens, and this
//! box leads alone. On again, it rejoins and the election settles as usual.
//!
//! Who may call it: the cluster token, a cluster client certificate, or the
//! settings token (`settings_token_file`), which opens nothing else.

use std::{path::PathBuf, process::Command, sync::Arc};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::{
    bootstrap::firewall::{self, FirewallSpec, NFT_TABLE},
    config::Config,
    discovery::DiscoveryEngine,
};

/// Comment on the nft accept `mesh_routed` adds; how it is found again.
pub const ROUTED_TAG: &str = "xdebbie mesh_routed";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub mesh_routed: bool,
    pub discovery: bool,
}

/// A `PUT`: either key, or both.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub mesh_routed: Option<bool>,
    pub discovery: Option<bool>,
}

pub fn path_for(cfg: &Config) -> PathBuf {
    cfg.state_file.with_file_name("settings.json")
}

fn read_saved(cfg: &Config) -> Option<Settings> {
    serde_json::from_str(&std::fs::read_to_string(path_for(cfg)).ok()?).ok()
}

/// Lay what was last set over the config file.
pub fn apply_saved(cfg: &mut Config) {
    if let Some(s) = read_saved(cfg) {
        cfg.enable_discovery = s.discovery;
        if let Some(n) = cfg.network.as_mut() {
            n.mesh_routed = s.mesh_routed;
        }
    }
}

/// What `cfg` (already overlaid) says now.
pub fn current(cfg: &Config) -> Settings {
    Settings {
        mesh_routed: cfg.network.as_ref().is_some_and(|n| n.mesh_routed),
        discovery: cfg.enable_discovery,
    }
}

/// The ufw rules (as argument lists) that `on` has and `off` lacks.
pub fn rule_diff(on: &FirewallSpec, off: &FirewallSpec) -> Vec<Vec<String>> {
    let off = off.rules();
    on.rules()
        .into_iter()
        .filter(|r| !off.contains(r))
        .collect()
}

/// The firewall with one knob forced.
fn spec_with(base: &FirewallSpec, s: Settings) -> FirewallSpec {
    let mut f = base.clone();
    if !s.discovery {
        f.discovery_port = None;
    }
    if let Some(e) = f.egress.as_mut()
        && !s.mesh_routed
    {
        e.routed_from = None;
    }
    f
}

pub struct SettingsCtl {
    path: PathBuf,
    /// The firewall with both knobs on: what each knob may add.
    full: FirewallSpec,
    engine: Option<Arc<DiscoveryEngine>>,
    now: Mutex<Settings>,
    /// Touch the host (ufw, nft). Off in tests.
    host: bool,
}

impl SettingsCtl {
    pub fn new(cfg: &Config, engine: Option<Arc<DiscoveryEngine>>) -> Self {
        let mut on = cfg.clone();
        on.enable_discovery = true;
        if let Some(n) = on.network.as_mut() {
            n.mesh_routed = true;
        }
        Self {
            path: path_for(cfg),
            full: FirewallSpec::from_config(&on),
            engine,
            now: Mutex::new(current(cfg)),
            host: true,
        }
    }

    #[cfg(test)]
    pub fn in_memory(full: FirewallSpec, now: Settings) -> Self {
        Self {
            path: std::env::temp_dir()
                .join(format!("xdebbie-settings-{}.json", uuid::Uuid::new_v4())),
            full,
            engine: None,
            now: Mutex::new(now),
            host: false,
        }
    }

    /// Drive `engine` as the daemon's does (discovery/sim_tests.rs).
    #[cfg(test)]
    pub fn with_engine(mut self, engine: Arc<DiscoveryEngine>) -> Self {
        self.engine = Some(engine);
        self
    }

    pub async fn get(&self) -> Settings {
        *self.now.lock().await
    }

    /// Apply `c`, persist, and return the result. Knobs that do not change
    /// do nothing.
    pub async fn set(&self, c: Change) -> Result<Settings> {
        let mut now = self.now.lock().await;
        let mut next = *now;
        if let Some(v) = c.mesh_routed {
            if v && self
                .full
                .egress
                .as_ref()
                .and_then(|e| e.routed_from.as_ref())
                .is_none()
            {
                bail!("mesh_routed needs [network] and the managed WireGuard mesh on this box");
            }
            next.mesh_routed = v;
        }
        if let Some(v) = c.discovery {
            if v && self.engine.is_none() && self.host {
                bail!(
                    "discovery cannot start here: the multicast socket did not open at startup (see the log)"
                );
            }
            next.discovery = v;
        }
        if next.mesh_routed != now.mesh_routed {
            self.flip(
                *now,
                Settings {
                    mesh_routed: next.mesh_routed,
                    ..*now
                },
            )?;
            if self.host {
                set_routed_nft(&self.full, next.mesh_routed)?;
            }
            info!(on = next.mesh_routed, "settings: mesh_routed");
        }
        if next.discovery != now.discovery {
            self.flip(
                Settings {
                    mesh_routed: next.mesh_routed,
                    ..*now
                },
                next,
            )?;
            if let Some(e) = &self.engine {
                e.set_enabled(next.discovery);
            }
            info!(on = next.discovery, "settings: discovery");
        }
        let body = serde_json::to_string_pretty(&next)? + "\n";
        crate::token::write_group_readable(&self.path, &body)
            .with_context(|| format!("saving {}", self.path.display()))?;
        *now = next;
        Ok(next)
    }

    /// Add (or delete) the ufw rules that differ between `from` and `to`.
    fn flip(&self, from: Settings, to: Settings) -> Result<()> {
        let (a, b) = (spec_with(&self.full, from), spec_with(&self.full, to));
        let (add, del) = (rule_diff(&b, &a), rule_diff(&a, &b));
        if !self.host {
            return Ok(());
        }
        for r in add {
            firewall::ufw_run(&r)?;
        }
        for r in del {
            let mut d = vec!["delete".to_string()];
            d.extend(r);
            firewall::ufw_run(&d)?;
        }
        Ok(())
    }
}

/// The one nft accept `mesh_routed` owns: delete every copy, then insert
/// one ahead of the drop when on. Nothing else in the table is touched.
fn set_routed_nft(full: &FirewallSpec, on: bool) -> Result<()> {
    let list = Command::new("nft")
        .args(["-a", "list", "chain", "ip", NFT_TABLE, "forward"])
        .output()
        .context("nft list")?;
    if !list.status.success() {
        bail!(
            "nft list chain ip {NFT_TABLE} forward: {}",
            String::from_utf8_lossy(&list.stderr).trim()
        );
    }
    for h in tagged_handles(&String::from_utf8_lossy(&list.stdout)) {
        nft(&["delete", "rule", "ip", NFT_TABLE, "forward", "handle", &h])?;
    }
    if on {
        let rule = firewall::routed_rule(full).context("no routed rule for this box")?;
        let mut a = vec!["insert", "rule", "ip", NFT_TABLE, "forward"];
        a.extend(rule.split_whitespace());
        nft(&a)?;
    }
    Ok(())
}

fn nft(a: &[&str]) -> Result<()> {
    let o = Command::new("nft")
        .args(a)
        .output()
        .context("running nft")?;
    if !o.status.success() {
        bail!(
            "nft {}: {}",
            a.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(())
}

/// Handles of rules carrying [`ROUTED_TAG`] in `nft -a list chain` output.
pub fn tagged_handles(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter(|l| l.contains(&format!("comment \"{ROUTED_TAG}\"")))
        .filter_map(|l| {
            l.rsplit_once("# handle ")
                .map(|(_, h)| h.trim().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::firewall::Egress;

    fn full() -> FirewallSpec {
        FirewallSpec {
            api_port: 23443,
            local_nets: vec!["10.22.0.0/16".into(), "192.168.1.0/24".into()],
            mesh_ifs: vec!["wg-flock".into(), "virbr-xdebbie".into()],
            discovery_port: Some(23442),
            egress: Some(Egress {
                bridge: "virbr-xdebbie".into(),
                bridge_addr: "10.23.0.1".parse().unwrap(),
                vm_net: "10.23.0.0/24".into(),
                upstream_dns: vec![],
                routed_from: Some(("wg-flock".into(), "10.22.0.0/16".into())),
            }),
            ..Default::default()
        }
    }

    fn joined(v: Vec<Vec<String>>) -> Vec<String> {
        v.into_iter().map(|r| r.join(" ")).collect()
    }

    #[test]
    fn each_knob_owns_only_its_rules() {
        let off = Settings::default();
        let routed = joined(rule_diff(
            &spec_with(
                &full(),
                Settings {
                    mesh_routed: true,
                    ..off
                },
            ),
            &spec_with(&full(), off),
        ));
        assert_eq!(
            routed,
            [
                "route allow in on wg-flock out on virbr-xdebbie from 10.22.0.0/16 comment xdebbie mesh 10.22.0.0/16 to vms via wg-flock"
            ]
        );
        let disc = joined(rule_diff(
            &spec_with(
                &full(),
                Settings {
                    discovery: true,
                    ..off
                },
            ),
            &spec_with(&full(), off),
        ));
        assert_eq!(disc.len(), 2, "{disc:#?}");
        assert!(
            disc.iter()
                .all(|r| r.contains("port 23442") && r.contains("xdebbie discovery")),
            "{disc:#?}"
        );
    }

    #[test]
    fn handles_are_found_by_tag() {
        let l = "table ip xdebbie_host {\n\tchain forward { # handle 2\n\
            \t\tiifname \"wg-flock\" ip saddr 10.22.0.0/16 oifname \"virbr-xdebbie\" counter packets 0 bytes 0 accept comment \"xdebbie mesh_routed\" # handle 9\n\
            \t\toifname \"virbr-xdebbie\" ip daddr 10.23.0.243 tcp dport { 80, 443 } counter accept comment \"aviary-nat\" # handle 11\n\
            \t\tiifname != \"virbr-xdebbie\" oifname \"virbr-xdebbie\" ct state new counter drop # handle 4\n";
        assert_eq!(tagged_handles(l), ["9"]);
    }

    #[tokio::test]
    async fn set_persists_and_refuses_routed_without_network() {
        let c = SettingsCtl::in_memory(full(), Settings::default());
        let s = c
            .set(Change {
                mesh_routed: Some(true),
                discovery: None,
            })
            .await
            .unwrap();
        assert_eq!(
            s,
            Settings {
                mesh_routed: true,
                discovery: false
            }
        );
        let saved: Settings =
            serde_json::from_str(&std::fs::read_to_string(&c.path).unwrap()).unwrap();
        assert_eq!(saved, s);
        let _ = std::fs::remove_file(&c.path);

        let mut bare = full();
        bare.egress = None;
        let c = SettingsCtl::in_memory(bare, Settings::default());
        assert!(
            c.set(Change {
                mesh_routed: Some(true),
                discovery: None
            })
            .await
            .is_err()
        );
        assert_eq!(c.get().await, Settings::default());
    }
}
