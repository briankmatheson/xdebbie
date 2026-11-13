// SPDX-License-Identifier: GPL-3.0-or-later
//! Followers shadow the leader's store.
//!
//! Everything goes through the leader, so its store is the truth and a
//! follower's stops moving while it follows. Left alone, a leader change
//! hands the cluster to a box that knows nothing about it. This task pulls
//! `/v1/hosts`, `/v1/vms` and `/v1/volumes` from the leader every few
//! seconds into the local store, so a new leader starts from a copy that
//! is seconds old, and every box holds the VM records its DHCP responder
//! answers from (`[network] leader_only = false`).
//!
//! Hosts named in this box's own `[[hosts]]` are kept as configured here
//! (local URI, local pool); the leader's view of them is the remote one.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use tracing::{info, warn};
use xdebbie_core::{Host, LeaderRef, Vm, Volume};

use crate::{config::Config, leader::Leadership, store::Store, token::Tokens};

const INTERVAL: Duration = Duration::from_secs(5);

pub struct Mirror {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub leadership: Leadership,
    pub http: reqwest::Client,
    pub tokens: Option<Arc<Tokens>>,
}

impl Mirror {
    pub async fn run(self) {
        if self.leadership.is_standalone() {
            return;
        }
        let mut from: Option<String> = None;
        let mut last_err: Option<String> = None;
        loop {
            tokio::time::sleep(INTERVAL).await;
            if self.leadership.is_leader().await {
                from = None;
                continue;
            }
            let Some(leader) = self.leadership.leader().await else {
                continue;
            };
            match self.pull(&leader).await {
                Ok(changed) => {
                    if from.as_deref() != Some(&leader.api_url) {
                        info!(leader = %leader.hostname, "mirroring the leader's store");
                        from = Some(leader.api_url.clone());
                    } else if changed {
                        tracing::debug!(leader = %leader.hostname, "mirror: store updated");
                    }
                    last_err = None;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if last_err.as_deref() != Some(&msg) {
                        warn!(leader = %leader.hostname, "mirroring the leader's store: {msg}");
                        last_err = Some(msg);
                    }
                }
            }
        }
    }

    /// One pull. Returns whether the local store changed.
    async fn pull(&self, leader: &LeaderRef) -> Result<bool> {
        let hosts: Vec<Host> = self.get(leader, "/v1/hosts").await?;
        let vms: Vec<Vm> = self.get(leader, "/v1/vms").await?;
        let volumes: Vec<Volume> = self.get(leader, "/v1/volumes").await?;
        // Leadership may have moved while we fetched: never write over a
        // store the reconciler has started driving.
        if self.leadership.is_leader().await {
            return Ok(false);
        }
        let mine: Vec<String> = self.cfg.hosts.iter().map(|h| h.name.clone()).collect();
        let cur = self.store.read().await;
        let mut next_hosts = BTreeMap::new();
        for h in hosts {
            if !mine.contains(&h.name) {
                next_hosts.insert(h.name.clone(), h);
            }
        }
        for n in &mine {
            if let Some(h) = cur.hosts.get(n) {
                next_hosts.insert(n.clone(), h.clone());
            }
        }
        let next_vms: BTreeMap<_, _> = vms.into_iter().map(|v| (v.id, v)).collect();
        let next_vols: BTreeMap<_, _> = volumes.into_iter().map(|v| (v.id.clone(), v)).collect();
        if cur.hosts == next_hosts && cur.vms == next_vms && cur.volumes == next_vols {
            return Ok(false);
        }
        self.store
            .update(move |s| {
                s.hosts = next_hosts;
                s.vms = next_vms;
                s.volumes = next_vols;
            })
            .await?;
        Ok(true)
    }

    async fn get<T: DeserializeOwned>(&self, leader: &LeaderRef, path: &str) -> Result<T> {
        let url = format!("{}{path}", leader.api_url.trim_end_matches('/'));
        let mut r = self.http.get(&url);
        if let Some(t) = &self.tokens {
            r = r.bearer_auth(t.get().await.current);
        }
        let resp = r.send().await.with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!(
                "GET {url}: {status}: {}",
                resp.text().await.unwrap_or_default().trim()
            );
        }
        resp.json()
            .await
            .with_context(|| format!("GET {url}: parsing"))
    }
}
