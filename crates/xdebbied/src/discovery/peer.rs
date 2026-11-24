// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;
// tokio's clock: the real one in the daemon, a paused, hand-advanced one in
// the simulated-LAN tests (discovery/sim.rs).
use tokio::time::Instant;
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
    pub fn new(node_id: Uuid, hostname: String, api_url: String, score: f64) -> Self {
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

    /// An announce: alive, with a new score. Says nothing about leadership,
    /// so the mark from the last heartbeat stays.
    pub fn update_announce(&mut self, score: f64, sequence: u64) {
        self.score = score;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u128) -> PeerInfo {
        PeerInfo::new(Uuid::from_u128(n), format!("p{n}"), "http://x".into(), 1.0)
    }

    #[tokio::test(start_paused = true)]
    async fn stale_peers_are_removed() {
        let mut t = PeerTable::new();
        t.insert(peer(1));
        t.insert(peer(2));
        tokio::time::advance(Duration::from_secs(10)).await;
        t.get_mut(&Uuid::from_u128(2))
            .unwrap()
            .update_heartbeat(1.0, false, 2);
        tokio::time::advance(Duration::from_secs(6)).await;
        let gone = t.remove_stale(Duration::from_secs(15));
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].hostname, "p1");
        assert_eq!(t.peers().count(), 1);
        // Discovery off drops everything.
        assert_eq!(t.remove_stale(Duration::ZERO).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn announce_keeps_the_leader_mark_heartbeat_moves_it() {
        let mut t = PeerTable::new();
        t.insert(peer(1));
        let p = t.get_mut(&Uuid::from_u128(1)).unwrap();
        p.update_heartbeat(5.0, true, 1);
        tokio::time::advance(Duration::from_secs(3)).await;
        p.update_announce(6.0, 2);
        assert!(p.is_leader);
        assert_eq!((p.score, p.last_sequence), (6.0, 2));
        assert!(!p.is_stale(Duration::from_secs(1)));
        assert!(t.current_leader().is_some());
        t.get_mut(&Uuid::from_u128(1))
            .unwrap()
            .update_heartbeat(6.0, false, 3);
        assert!(t.current_leader().is_none());
    }
}
