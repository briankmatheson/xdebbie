// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Information about a discovered peer node
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub node_id: Uuid,
    pub hostname: String,
    pub api_url: String,
    pub score: f64,
    pub is_leader: bool,
    pub last_seen: Instant,
    pub last_sequence: u64,
}

impl PeerInfo {
    pub fn new(
        node_id: Uuid,
        hostname: String,
        api_url: String,
        score: f64,
    ) -> Self {
        Self {
            node_id,
            hostname,
            api_url,
            score,
            is_leader: false,
            last_seen: Instant::now(),
            last_sequence: 0,
        }
    }

    pub fn update_heartbeat(&mut self, score: f64, is_leader: bool, sequence: u64) {
        self.score = score;
        self.is_leader = is_leader;
        self.last_seen = Instant::now();
        self.last_sequence = sequence;
    }

    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_seen.elapsed() > timeout
    }
}

/// Peer table managing all discovered nodes
#[derive(Debug, Default)]
pub struct PeerTable {
    peers: std::collections::HashMap<Uuid, PeerInfo>,
}

impl PeerTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, peer: PeerInfo) {
        self.peers.insert(peer.node_id, peer);
    }

    pub fn get(&self, node_id: &Uuid) -> Option<&PeerInfo> {
        self.peers.get(node_id)
    }

    pub fn get_mut(&mut self, node_id: &Uuid) -> Option<&mut PeerInfo> {
        self.peers.get_mut(node_id)
    }

    pub fn peers(&self) -> impl Iterator<Item = &PeerInfo> {
        self.peers.values()
    }

    pub fn remove_stale(&mut self, timeout: Duration) -> Vec<PeerInfo> {
        let stale_ids: Vec<Uuid> = self
            .peers
            .values()
            .filter(|p| p.is_stale(timeout))
            .map(|p| p.node_id)
            .collect();

        stale_ids
            .into_iter()
            .filter_map(|id| self.peers.remove(&id))
            .collect()
    }

    pub fn current_leader(&self) -> Option<&PeerInfo> {
        self.peers.values().find(|p| p.is_leader)
    }
}
