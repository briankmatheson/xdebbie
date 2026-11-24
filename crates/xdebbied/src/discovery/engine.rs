// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
// tokio's clock, so the simulated-LAN tests (sim.rs) can run it paused.
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

use super::{
    messages::{DiscoveryMessage, NodeCapacity},
    multicast::{DEFAULT_MULTICAST_ADDR, MulticastSocket, Transport},
    peer::{PeerInfo, PeerTable},
    wire,
};
use crate::election::{
    scoring::{calculate_score, get_uptime},
    state::{ElectionState, ElectionStateMachine},
};

pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
pub const PEER_TIMEOUT: Duration = Duration::from_secs(15);
pub const ELECTION_TIMEOUT: Duration = Duration::from_secs(10);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a sender's last sequence number is remembered after it falls
/// silent. A captured datagram verifies for as long as its timestamp is
/// within [`wire::MAX_SKEW`] of the receiver's clock, and the sender's clock
/// may itself be up to that far off: so at most twice that after it was
/// sent, plus a margin.
const REPLAY_MEMORY: Duration = Duration::from_secs(2 * wire::MAX_SKEW.as_secs() + 15);

/// What the engine reads from the box it runs on. [`DiscoveryEngine::new`]
/// uses the real box ([`RealHost`]); the simulated-LAN tests substitute
/// their own clocks and hardware.
pub trait Host: Send + Sync {
    /// How long the box has been up (feeds the election score).
    fn uptime(&self) -> Duration;
    /// This box's capacity, with `vm_count` VMs in its store.
    fn capacity(&self, vm_count: usize) -> NodeCapacity;
    /// The wall clock in Unix seconds: signing timestamps and the replay
    /// window (wire.rs).
    fn wall_secs(&self) -> u64;
    /// Does a peer's hostname resolve here? May block on DNS.
    fn resolves(&self, host: &str) -> bool;
}

/// The box the daemon runs on.
pub struct RealHost;

impl Host for RealHost {
    fn uptime(&self) -> Duration {
        get_uptime()
    }

    /// Placeholder - should integrate with actual scheduler
    fn capacity(&self, vm_count: usize) -> NodeCapacity {
        // TODO: Get real capacity from scheduler/store
        NodeCapacity {
            total_cpus: num_cpus::get() as u32,
            total_memory_mib: 65536, // Placeholder
            available_cpus: num_cpus::get() as f32,
            available_memory_mib: 32768, // Placeholder
            vm_count,
            control_plane_eligible: true,
        }
    }

    fn wall_secs(&self) -> u64 {
        wire::now()
    }

    fn resolves(&self, host: &str) -> bool {
        super::reach::resolves(host)
    }
}

/// The engine's ties to the outside world, for
/// [`DiscoveryEngine::with_parts`].
pub struct Parts {
    /// Fresh per process in the daemon.
    pub node_id: Uuid,
    pub transport: Box<dyn Transport>,
    pub host: Arc<dyn Host>,
}

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

/// Did a message just start an election here (we outrank a proposer or a
/// leader)? Then we must say so: a candidate nobody hears from wins nothing,
/// and the weaker side would only learn of us after the election timeout,
/// when every candidate declares itself leader at once.
fn entered_election(before: ElectionState, after: ElectionState) -> bool {
    before != ElectionState::Election && after == ElectionState::Election
}

/// Discovery engine managing peer discovery and leader election
pub struct DiscoveryEngine {
    node_id: Uuid,
    config: DiscoveryConfig,
    transport: Box<dyn Transport>,
    host: Arc<dyn Host>,
    peers: Arc<RwLock<PeerTable>>,
    election: Arc<RwLock<ElectionStateMachine>>,
    start_time: Instant,
    /// Sources already warned about, so a stray sender logs once, not every second.
    warned: std::sync::Mutex<std::collections::HashSet<std::net::IpAddr>>,
    /// The highest sequence number heard from each sender, and when. A
    /// datagram that does not raise it is a replay (or a duplicate).
    seen: std::sync::Mutex<std::collections::HashMap<Uuid, (u64, Instant)>>,
    /// Discovery switched on (`PUT /v1/settings`). Off: send nothing, drop
    /// what arrives, forget the peers, and lead alone.
    enabled: std::sync::atomic::AtomicBool,
}

impl DiscoveryEngine {
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
        let bind_addr = match &config.bind_addr {
            Some(b) => b.clone(),
            None => {
                let port = config.multicast_addr.rsplit(':').next().unwrap_or("23442");
                format!("0.0.0.0:{port}")
            }
        };
        let socket = MulticastSocket::new(&config.multicast_addr, &bind_addr)?;
        Ok(Self::with_parts(
            config,
            Parts {
                node_id: Uuid::new_v4(),
                transport: Box::new(socket),
                host: Arc::new(RealHost),
            },
        ))
    }

    /// An engine on the given transport and host.
    pub fn with_parts(mut config: DiscoveryConfig, parts: Parts) -> Self {
        let Parts {
            node_id,
            transport,
            host,
        } = parts;
        if config.addrs.is_empty() {
            config.addrs = super::reach::local_addrs(&config.multicast_addr);
        }
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

        Self {
            node_id,
            enabled: std::sync::atomic::AtomicBool::new(config.enable_discovery),
            config,
            transport,
            host,
            peers,
            election,
            start_time: Instant::now(),
            warned: Default::default(),
            seen: Default::default(),
        }
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
        let bytes = wire::seal_at(
            msg,
            self.signing_key().await.as_ref(),
            self.host.wall_secs(),
        )?;
        self.transport.send(&bytes)
    }

    /// The message in a datagram if it is welcome here: signed with our
    /// token, or unsigned where that is allowed.
    async fn admit(&self, bytes: &[u8], src: std::net::SocketAddr) -> Option<DiscoveryMessage> {
        let keys = self.verify_keys().await;
        let why = match wire::open_at(bytes, &keys, self.host.wall_secs()) {
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

    /// Is `msg` newer than anything heard from its sender? Signatures and
    /// the clock window stop forgeries and old captures; this stops a
    /// datagram captured in the last minute from being played back, which
    /// could keep a dead leader looking alive or a dead peer listed.
    fn fresh(&self, msg: &DiscoveryMessage) -> bool {
        let Ok(mut seen) = self.seen.lock() else {
            return true;
        };
        let (id, seq) = (msg.node_id(), msg.sequence());
        match seen.get(&id) {
            Some((last, _)) if seq <= *last => false,
            _ => {
                seen.insert(id, (seq, Instant::now()));
                true
            }
        }
    }

    fn get_capacity(&self) -> NodeCapacity {
        self.host.capacity(
            self.config
                .vm_count
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Calculate our current score. Compare only against this, freshly
    /// computed: the copy in the election state machine is from our last
    /// heartbeat, and lacks the leader bonus for up to a second after we win.
    async fn calculate_our_score(&self) -> f64 {
        let capacity = self.get_capacity();
        let uptime = self.host.uptime().max(self.start_time.elapsed());
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
                    peer.update_announce(score, sequence);
                } else {
                    // New peer: make sure we can actually dial what it
                    // announced. Resolver runs off the runtime; see reach.rs.
                    let candidates: Vec<_> = super::reach::source_ip(src)
                        .into_iter()
                        .chain(addrs)
                        .collect();
                    let host = self.host.clone();
                    let api_url = super::reach::reachable_url_async(
                        api_url,
                        candidates,
                        Arc::new(move |h: &str| host.resolves(h)),
                    )
                    .await;
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
                    let mine = self.calculate_our_score().await;
                    let mut election = self.election.write().await;
                    election.update_score(mine);
                    let before = election.state();
                    election.received_leader_heartbeat(node_id, score);
                    let challenge = entered_election(before, election.state());
                    drop(election);
                    if challenge {
                        self.send_election_propose().await?;
                    }
                }
            }

            DiscoveryMessage::ElectionPropose {
                node_id,
                score,
                sequence: _,
            } => {
                let mine = self.calculate_our_score().await;
                let mut election = self.election.write().await;
                election.update_score(mine);
                let before = election.state();
                let should_vote = election.received_election_propose(node_id, score);
                let challenge = entered_election(before, election.state());
                drop(election);

                if should_vote {
                    self.send_vote(node_id).await?;
                }
                if challenge {
                    self.send_election_propose().await?;
                }
            }

            DiscoveryMessage::ElectionVote {
                node_id: _,
                voted_for,
                sequence: _,
            } => {
                if voted_for == self.node_id {
                    let mine = self.calculate_our_score().await;
                    let mut election = self.election.write().await;
                    election.update_score(mine);
                    election.received_vote(msg.node_id());

                    // Check if we won the election
                    let peers = self.peers.read().await;
                    let total_nodes = peers.peers().count() + 1; // +1 for ourselves
                    // Votes only say the voters rank below us: a box that
                    // outranks half the cluster collects a majority. Not a
                    // win while a box we heard from lately outranks us; it
                    // will campaign (it heard our proposal) and we will vote
                    // for it.
                    let better = peers
                        .peers()
                        .find(|p| {
                            p.last_seen.elapsed() <= HEARTBEAT_TIMEOUT
                                && election.outranked_by(p.score, p.node_id)
                        })
                        .map(|p| p.hostname.clone());
                    drop(peers);

                    if election.has_majority(total_nodes) {
                        match better {
                            Some(b) => tracing::debug!(
                                "majority of votes, but {b} outranks us; not taking the lead"
                            ),
                            None => {
                                info!("Won election with majority vote, becoming leader");
                                election.become_leader();
                            }
                        }
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

        // Send initial announce. Not with discovery off: peers would list
        // this box for 15 s and a bootstrap listening nearby would refuse
        // to start a cluster because of it.
        if self.enabled()
            && let Err(e) = self.send_announce().await
        {
            warn!("Failed to send initial announce: {}", e);
        }

        loop {
            if !self.enabled() {
                // Paused: lead alone, hear nobody. Checked every tick, so a
                // switch back on rejoins within a second.
                heartbeat_ticker.tick().await;
                while let Ok(Some(_)) = self.transport.recv() {}
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
                    if let Ok(mut seen) = self.seen.lock() {
                        seen.retain(|_, (_, at)| at.elapsed() <= REPLAY_MEMORY);
                    }
                }

                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Process incoming messages
                    while let Ok(Some((bytes, addr))) = self.transport.recv() {
                        let Some(msg) = self.admit(&bytes, addr).await else {
                            continue;
                        };
                        if msg.node_id() != self.node_id && !self.fresh(&msg) {
                            tracing::debug!(src = %addr, node = %msg.node_id(), seq = msg.sequence(), "discovery: dropping replayed or duplicate datagram");
                            continue;
                        }
                        if let Err(e) = self.process_message(msg, addr).await {
                            warn!("Failed to process message: {}", e);
                        }
                    }
                }
            }
        }
    }
}
