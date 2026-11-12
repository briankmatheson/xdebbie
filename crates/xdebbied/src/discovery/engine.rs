// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use super::{
    messages::{DiscoveryMessage, NodeCapacity},
    multicast::{MulticastSocket, DEFAULT_MULTICAST_ADDR},
    peer::{PeerInfo, PeerTable},
};
use crate::election::{
    scoring::{calculate_score, get_uptime},
    state::{ElectionState, ElectionStateMachine},
};

const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
const ELECTION_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct DiscoveryConfig {
    pub multicast_addr: String,
    /// Where to receive; defaults to every interface on the multicast port.
    pub bind_addr: Option<String>,
    pub hostname: String,
    pub api_url: String,
    pub enable_discovery: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            multicast_addr: DEFAULT_MULTICAST_ADDR.to_string(),
            bind_addr: None,
            hostname: hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string()),
            api_url: "http://127.0.0.1:7443".to_string(),
            enable_discovery: true,
        }
    }
}

/// Discovery engine managing peer discovery and leader election
pub struct DiscoveryEngine {
    node_id: Uuid,
    config: DiscoveryConfig,
    socket: MulticastSocket,
    peers: Arc<RwLock<PeerTable>>,
    election: Arc<RwLock<ElectionStateMachine>>,
    start_time: Instant,
}

impl DiscoveryEngine {
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
        let node_id = Uuid::new_v4();
        let bind_addr = match &config.bind_addr {
            Some(b) => b.clone(),
            None => {
                let port = config.multicast_addr.rsplit(':').next().unwrap_or("7442");
                format!("0.0.0.0:{port}")
            }
        };
        let socket = MulticastSocket::new(&config.multicast_addr, &bind_addr)?;
        let peers = Arc::new(RwLock::new(PeerTable::new()));
        let election = Arc::new(RwLock::new(ElectionStateMachine::new(
            node_id,
            ELECTION_TIMEOUT,
            HEARTBEAT_TIMEOUT,
        )));

        info!(
            "Discovery engine initialized: node_id={} multicast={}",
            node_id, config.multicast_addr
        );

        Ok(Self {
            node_id,
            config,
            socket,
            peers,
            election,
            start_time: Instant::now(),
        })
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    pub fn peers(&self) -> Arc<RwLock<PeerTable>> {
        self.peers.clone()
    }

    pub fn election(&self) -> Arc<RwLock<ElectionStateMachine>> {
        self.election.clone()
    }

    /// Get current node capacity (placeholder - should integrate with actual scheduler)
    fn get_capacity(&self) -> NodeCapacity {
        // TODO: Get real capacity from scheduler/store
        NodeCapacity {
            total_cpus: num_cpus::get() as u32,
            total_memory_mib: 65536, // Placeholder
            available_cpus: num_cpus::get() as f32,
            available_memory_mib: 32768, // Placeholder
            vm_count: 0,
            control_plane_eligible: true,
        }
    }

    /// Calculate our current score
    async fn calculate_our_score(&self) -> f64 {
        let capacity = self.get_capacity();
        let uptime = get_uptime().max(self.start_time.elapsed());
        let was_leader = self.election.read().await.is_leader();

        calculate_score(&capacity, uptime, was_leader, self.node_id)
    }

    /// Send announce message
    async fn send_announce(&self) -> Result<()> {
        let capacity = self.get_capacity();
        let score = self.calculate_our_score().await;
        let mut election = self.election.write().await;
        election.update_score(score);
        let sequence = election.next_sequence();
        drop(election);

        let msg = DiscoveryMessage::Announce {
            node_id: self.node_id,
            hostname: self.config.hostname.clone(),
            api_url: self.config.api_url.clone(),
            capacity,
            score,
            sequence,
        };

        self.socket.send(&msg)?;
        Ok(())
    }

    /// Send heartbeat message
    async fn send_heartbeat(&self) -> Result<()> {
        let score = self.calculate_our_score().await;
        let mut election = self.election.write().await;
        election.update_score(score);
        let is_leader = election.is_leader();
        let sequence = election.next_sequence();
        drop(election);

        let msg = DiscoveryMessage::Heartbeat {
            node_id: self.node_id,
            score,
            is_leader,
            sequence,
        };

        self.socket.send(&msg)?;
        Ok(())
    }

    /// Send election proposal
    async fn send_election_propose(&self) -> Result<()> {
        let score = self.calculate_our_score().await;
        let mut election = self.election.write().await;
        let sequence = election.next_sequence();
        drop(election);

        let msg = DiscoveryMessage::ElectionPropose {
            node_id: self.node_id,
            score,
            sequence,
        };

        self.socket.send(&msg)?;
        Ok(())
    }

    /// Send election vote
    async fn send_vote(&self, voted_for: Uuid) -> Result<()> {
        let mut election = self.election.write().await;
        let sequence = election.next_sequence();
        drop(election);

        let msg = DiscoveryMessage::ElectionVote {
            node_id: self.node_id,
            voted_for,
            sequence,
        };

        self.socket.send(&msg)?;
        Ok(())
    }

    /// Process received message
    async fn process_message(&self, msg: DiscoveryMessage) -> Result<()> {
        // Ignore our own messages
        if msg.node_id() == self.node_id {
            return Ok(());
        }

        match msg {
            DiscoveryMessage::Announce {
                node_id,
                hostname,
                api_url,
                capacity: _,
                score,
                sequence,
            } => {
                let mut peers = self.peers.write().await;
                if let Some(peer) = peers.get_mut(&node_id) {
                    peer.update_heartbeat(score, false, sequence);
                } else {
                    let peer = PeerInfo::new(node_id, hostname, api_url, score);
                    info!("Discovered new peer: {} ({})", peer.hostname, peer.node_id);
                    peers.insert(peer);
                }
            }

            DiscoveryMessage::Heartbeat {
                node_id,
                score,
                is_leader,
                sequence,
            } => {
                let mut peers = self.peers.write().await;
                if let Some(peer) = peers.get_mut(&node_id) {
                    peer.update_heartbeat(score, is_leader, sequence);
                }
                drop(peers);

                if is_leader {
                    let mut election = self.election.write().await;
                    election.received_leader_heartbeat(node_id, score);
                }
            }

            DiscoveryMessage::ElectionPropose {
                node_id,
                score,
                sequence: _,
            } => {
                let mut election = self.election.write().await;
                let should_vote = election.received_election_propose(node_id, score);
                drop(election);

                if should_vote {
                    self.send_vote(node_id).await?;
                }
            }

            DiscoveryMessage::ElectionVote {
                node_id: _,
                voted_for,
                sequence: _,
            } => {
                if voted_for == self.node_id {
                    let mut election = self.election.write().await;
                    election.received_vote(msg.node_id());

                    // Check if we won the election
                    let peers = self.peers.read().await;
                    let total_nodes = peers.peers().count() + 1; // +1 for ourselves
                    drop(peers);

                    if election.has_majority(total_nodes) {
                        info!("Won election with majority vote, becoming leader");
                        election.become_leader();
                    }
                }
            }
        }

        Ok(())
    }

    /// Main discovery loop
    pub async fn run(self: Arc<Self>) -> Result<()> {
        if !self.config.enable_discovery {
            info!("Discovery disabled, running in standalone mode");
            return Ok(());
        }

        info!("Starting discovery engine");

        // Activate election state machine
        self.election.write().await.activate();

        let mut announce_ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
        let mut heartbeat_ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
        let mut cleanup_ticker = tokio::time::interval(Duration::from_secs(10));

        // Send initial announce
        if let Err(e) = self.send_announce().await {
            warn!("Failed to send initial announce: {}", e);
        }

        loop {
            tokio::select! {
                _ = announce_ticker.tick() => {
                    if let Err(e) = self.send_announce().await {
                        warn!("Failed to send announce: {}", e);
                    }
                }

                _ = heartbeat_ticker.tick() => {
                    if let Err(e) = self.send_heartbeat().await {
                        warn!("Failed to send heartbeat: {}", e);
                    }

                    // Check for election conditions
                    let mut election = self.election.write().await;

                    if election.state() == ElectionState::Follower && election.leader_heartbeat_timeout() {
                        info!("Leader heartbeat timeout, starting election");
                        election.start_election();
                        drop(election);
                        if let Err(e) = self.send_election_propose().await {
                            warn!("Failed to send election proposal: {}", e);
                        }
                    } else if election.state() == ElectionState::Election && election.election_timeout() {
                        warn!("Election timeout, becoming leader by default");
                        election.become_leader();
                    }
                }

                _ = cleanup_ticker.tick() => {
                    let mut peers = self.peers.write().await;
                    let stale = peers.remove_stale(PEER_TIMEOUT);
                    for peer in stale {
                        info!("Removed stale peer: {} ({})", peer.hostname, peer.node_id);
                    }
                }

                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Process incoming messages
                    while let Ok(Some((msg, _addr))) = self.socket.recv() {
                        if let Err(e) = self.process_message(msg).await {
                            warn!("Failed to process message: {}", e);
                        }
                    }
                }
            }
        }
    }
}
