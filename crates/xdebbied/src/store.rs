// SPDX-License-Identifier: GPL-3.0-or-later
//! Durable state: one JSON document, rewritten atomically on every change.
//! Small clouds have tens of hosts and hundreds of VMs; this is plenty.

use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};

use xdebbie_core::{Host, Vm, Volume};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    pub hosts: BTreeMap<String, Host>,
    pub vms: BTreeMap<Uuid, Vm>,
    /// Standalone volumes keyed by ABE id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, Volume>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<RwLock<State>>,
    path: Arc<PathBuf>,
}

impl Store {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let state = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(state)),
            path: Arc::new(path),
        })
    }

    pub async fn read(&self) -> State {
        self.inner.read().await.clone()
    }

    /// Mutate under the write lock and persist before releasing it, so no
    /// reader ever observes state that is not on disk.
    pub async fn update<R>(&self, f: impl FnOnce(&mut State) -> R) -> anyhow::Result<R> {
        let mut g = self.inner.write().await;
        let mut next = g.clone();
        let r = f(&mut next);
        persist(&self.path, &next)?;
        *g = next;
        Ok(r)
    }
}

fn persist(path: &std::path::Path, s: &State) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec_pretty(s)?)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}
