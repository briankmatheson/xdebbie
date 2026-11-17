// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use super::{
    messages::{DiscoveryMessage, NodeCapacity},
    multicast::{DEFAULT_MULTICAST_ADDR, MulticastSocket},
    peer::{PeerInfo, PeerTable},
    wire,
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
    /// Addresses announced alongside `api_url`; filled by
    /// [`reach::local_addrs`] when left empty.
    pub addrs: Vec<std::net::IpAddr>,
    /// VMs in this box's store, kept current by the caller. Feeds the
    /// election score so the box holding the state leads.
    pub vm_count: Arc<std::sync::atomic::AtomicUsize>,
    pub enable_discovery: bool,
    /// The cluster token set. With it, everything sent is signed and only
    /// what verifies is heard (wire.rs); without it (an unauthenticated
    /// API) messages go out plain.
    pub tokens: Option<Arc<crate::token::Tokens>>,
    /// Also hear unsigned messages: for a rolling upgrade, while some boxes
    /// still run a daemon that does not sign. Off by default.
    pub accept_unsigned: bool,
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
            api_url: "http://127.0.0.1:23443".to_string(),
            addrs: vec![],
            vm_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            enable_discovery: true,
            tokens: None,
            accept_unsigned: false,
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
    /// Sources already warned about, so a stray sender logs once, not every second.
    warned: std::sync::Mutex<std::collections::HashSet<std::net::IpAddr>>,
    /// Discovery switched on (`PUT /v1/settings`). Off: send nothing, drop
    /// what arrives, forget the peers, and lead alone.
    enabled: std::sync::atomic::AtomicBool,
}

impl DiscoveryEngine {
    pub fn new(mut config: DiscoveryConfig) -> Result<Self> {
        let node_id = Uuid::new_v4();
        if config.addrs.is_empty() {
            config.addrs = super::reach::local_addrs(&config.multicast_addr);
        }
        let bind_addr = match &config.bind_addr {
            Some(b) => b.clone(),
            None => {
                let port = config.multicast_addr.rsplit(':').next().unwrap_or("23442");
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
            enabled: std::sync::atomic::AtomicBool::new(config.enable_discovery),
            config,
            socket,
            peers,
            election,
            start_time: Instant::now(),
            warned: Default::default(),
        })
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
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

    /// Signing key: the *previous* token's while a rotation is in flight,
    /// since a box the leader has not reached yet holds only that one;
    /// otherwise the current token's. Receivers try both.
    async fn signing_key(&self) -> Option<wire::Key> {
        let set = self.config.tokens.as_ref()?.get().await;
        Some(wire::Key::from_token(
            set.previous.as_deref().unwrap_or(&set.current),
        ))
    }

    async fn verify_keys(&self) -> Vec<wire::Key> {
        let Some(t) = &self.config.tokens else {
            return vec![];
        };
        let set = t.get().await;
        std::iter::once(set.current.as_str())
            .chain(set.previous.as_deref())
            .map(wire::Key::from_token)
            .collect()
    }

    async fn send(&self, msg: &DiscoveryMessage) -> Result<()> {
        let bytes = wire::seal(msg, self.signing_key().await.as_ref())?;
        self.socket.send(&bytes)
    }

    /// The message in a datagram if it is welcome here: signed with our
    /// token, or unsigned where that is allowed.
    async fn admit(&self, bytes: &[u8], src: std::net::SocketAddr) -> Option<DiscoveryMessage> {
        let keys = self.verify_keys().await;
        let why = match wire::open(bytes, &keys) {
            Ok(wire::Opened::Verified(m)) => return Some(m),
            Ok(wire::Opened::Unsigned(m)) => {
                if keys.is_empty() || self.config.accept_unsigned {
                    return Some(m);
                }
                "unsigned (a daemon that predates signing? set \
                 discovery_accept_unsigned = true while upgrading)"
                    .to_string()
            }
            Err(e) => format!("{e:#}"),
        };
        let first = self
            .warned
            .lock()
            .map(|mut w| w.insert(src.ip()))
            .unwrap_or(false);
        if first {
            warn!(%src, "discovery: dropping datagram: {why} (further drops from this address at debug)");
        } else {
            tracing::debug!(%src, "discovery: dropping datagram: {why}");
        }
        None
    }

    /// Get current node capacity (placeholder - should integrate with actual scheduler)
    fn get_capacity(&self) -> NodeCapacity {
        // TODO: Get real capacity from scheduler/store
        NodeCapacity {
            total_cpus: num_cpus::get() as u32,
            total_memory_mib: 65536, // Placeholder
            available_cpus: num_cpus::get() as f32,
            available_memory_mib: 32768, // Placeholder
            vm_count: self
                .config
                .vm_count
                .load(std::sync::atomic::Ordering::Relaxed),
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
            addrs: self.config.addrs.clone(),
            capacity,
            score,
            sequence,
        };

        self.send(&msg).await?;
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

        self.send(&msg).await?;
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

        self.send(&msg).await?;
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

        self.send(&msg).await?;
        Ok(())
    }

    /// Process received message
    async fn process_message(
        &self,
        msg: DiscoveryMessage,
        src: std::net::SocketAddr,
    ) -> Result<()> {
        // Ignore our own messages
        if msg.node_id() == self.node_id {
            return Ok(());
        }

        match msg {
            DiscoveryMessage::Announce {
                node_id,
                hostname,
                api_url,
                addrs,
                capacity: _,
                score,
                sequence,
            } => {
                if let Some(peer) = self.peers.write().await.get_mut(&node_id) {
                    peer.update_heartbeat(score, false, sequence);
                } else {
                    // New peer: make sure we can actually dial what it
                    // announced. Resolver runs off the runtime; see reach.rs.
                    let candidates: Vec<_> = super::reach::source_ip(src)
                        .into_iter()
                        .chain(addrs)
                        .collect();
                    let api_url = super::reach::reachable_url_async(api_url, candidates).await;
                    let mut peers = self.peers.write().await;
                    if peers.get_mut(&node_id).is_some() {
                        return Ok(());
                    }
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
        info!(enabled = self.enabled(), "Starting discovery engine");

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
            if !self.enabled() {
                // Paused: lead alone, hear nobody. Checked every tick, so a
                // switch back on rejoins within a second.
                heartbeat_ticker.tick().await;
                while let Ok(Some(_)) = self.socket.recv() {}
                let mut election = self.election.write().await;
                if !election.is_leader() {
                    info!("discovery off: leading alone");
                    election.become_leader();
                }
                drop(election);
                let mut peers = self.peers.write().await;
                for p in peers.remove_stale(Duration::ZERO) {
                    info!("discovery off: dropped peer {} ({})", p.hostname, p.node_id);
                }
                continue;
            }
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
                    while let Ok(Some((bytes, addr))) = self.socket.recv() {
                        let Some(msg) = self.admit(&bytes, addr).await else {
                            continue;
                        };
                        if let Err(e) = self.process_message(msg, addr).await {
                            warn!("Failed to process message: {}", e);
                        }
                    }
                }
            }
        }
    }
}
