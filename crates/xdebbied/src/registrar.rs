// SPDX-License-Identifier: GPL-3.0-or-later
//! Every box tells the leader about its own hypervisor.
//!
//! Stores are per box (docs/autodiscovery.md): a `[[hosts]]` entry seeds
//! the store of the daemon that read it, and nobody reads a follower's
//! store. `bootstrap --join` registers once, with the CA box; that is not
//! enough once leadership moves to a box that never saw the entry (the CA
//! box itself registers nowhere), so a VM pinned to such a host sits in
//! `Pending: pinned elsewhere`. This task re-registers with whoever leads,
//! every few seconds, until the leader has the entry as sent, and again
//! every minute in case the leader's store went backwards.
//!
//! What is sent is the local entry as another box must see it
//! ([`remote_view`]): `uri` rewritten to `qemu+tls://<libvirtd listen
//! address>/system`, and `agent` set to this daemon's API URL so the leader
//! hands host-local work to it (`agent.rs`) instead of driving libvirt over
//! TLS and the thin pool from the wrong box.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};
use xdebbie_core::{Host, LeaderRef};

use crate::{config::Config, leader::Leadership, libvirt::uri_is_local, token::Tokens};

const INTERVAL: Duration = Duration::from_secs(5);
/// Re-send even when nothing changed, so a leader that reloaded an older
/// store converges without a restart here.
const REFRESH: Duration = Duration::from_secs(60);

pub struct Registrar {
    pub cfg: Arc<Config>,
    pub leadership: Leadership,
    pub http: reqwest::Client,
    pub tokens: Option<Arc<Tokens>>,
    /// This daemon's API URL: what the leader will call back.
    pub api_url: String,
}

/// `[[hosts]]` entry as another box must see it: reachable `uri`, this
/// daemon as `agent`, runtime fields cleared (the leader's inventory loop
/// fills them).
pub fn remote_view(h: &Host, api_url: &str, libvirt_addr: &str) -> Host {
    let mut h = h.clone();
    if uri_is_local(&h.uri) {
        h.uri = format!("qemu+tls://{libvirt_addr}/system");
    }
    h.agent = Some(api_url.to_string());
    h.capacity = None;
    h.last_seen = None;
    h
}

impl Registrar {
    fn hosts(&self) -> Vec<Host> {
        // libvirtd listens on the overlay bridge address (bootstrap step 7);
        // the hostname is the fallback the join step uses too.
        let addr = crate::bootstrap::tls_certs::libvirtd_listen_addr()
            .unwrap_or_else(crate::leader::local_hostname);
        self.cfg
            .hosts
            .iter()
            .map(|h| remote_view(h, &self.api_url, &addr))
            .collect()
    }

    pub async fn run(self) {
        if self.leadership.is_standalone() {
            return;
        }
        // (leader, what it has from us, when): nothing to do while unchanged.
        let mut sent: Option<(String, Vec<Host>, std::time::Instant)> = None;
        let mut last_err: Option<String> = None;
        loop {
            tokio::time::sleep(INTERVAL).await;
            if self.leadership.is_leader().await {
                sent = None;
                continue;
            }
            let Some(leader) = self.leadership.leader().await else {
                continue;
            };
            let hosts = self.hosts();
            if let Some((url, h, at)) = &sent
                && *url == leader.api_url
                && *h == hosts
                && at.elapsed() < REFRESH
            {
                continue;
            }
            let mut all = true;
            for h in &hosts {
                match self.put(&leader, h).await {
                    Ok(()) => {
                        if sent.as_ref().is_none_or(|(u, _, _)| *u != leader.api_url) {
                            info!(host = %h.name, uri = %h.uri, leader = %leader.hostname, "registered this box's hypervisor with the leader");
                        }
                        last_err = None;
                    }
                    Err(e) => {
                        all = false;
                        let msg = format!("{e:#}");
                        if last_err.as_deref() != Some(&msg) {
                            warn!(host = %h.name, leader = %leader.hostname, "registering with the leader: {msg}");
                            last_err = Some(msg);
                        }
                    }
                }
            }
            sent = all.then(|| (leader.api_url.clone(), hosts, std::time::Instant::now()));
        }
    }

    async fn put(&self, leader: &LeaderRef, h: &Host) -> Result<()> {
        let url = format!(
            "{}/v1/hosts/{}",
            leader.api_url.trim_end_matches('/'),
            h.name
        );
        let mut r = self.http.put(&url).json(h);
        if let Some(t) = &self.tokens {
            r = r.bearer_auth(t.get().await.current);
        }
        let resp = r.send().await.with_context(|| format!("PUT {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!(
                "PUT {url}: {status}: {}",
                resp.text().await.unwrap_or_default().trim()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(uri: &str) -> Host {
        serde_json::from_value(
            serde_json::json!({"name": "box0", "uri": uri, "abe": "https://10.22.0.1:443"}),
        )
        .unwrap()
    }

    #[test]
    fn local_uri_becomes_tls_to_libvirtd_and_agent_is_set() {
        let h = remote_view(&host("qemu:///system"), "https://box0:7443", "10.23.0.1");
        assert_eq!(h.uri, "qemu+tls://10.23.0.1/system");
        assert_eq!(h.agent.as_deref(), Some("https://box0:7443"));
        assert_eq!(h.abe.as_deref(), Some("https://10.22.0.1:443"));
        assert!(h.capacity.is_none() && h.last_seen.is_none());
    }

    #[test]
    fn a_remote_uri_in_config_is_kept() {
        let h = remote_view(
            &host("qemu+ssh://xdebbie@203.0.113.9/system"),
            "https://box0:7443",
            "10.23.0.1",
        );
        assert_eq!(h.uri, "qemu+ssh://xdebbie@203.0.113.9/system");
        assert_eq!(h.agent.as_deref(), Some("https://box0:7443"));
    }
}
