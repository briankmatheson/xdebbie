// SPDX-License-Identifier: GPL-3.0-or-later
//! A simulated LAN for discovery and election tests.
//!
//! Every box is a real [`DiscoveryEngine`] running its real loop, wired to
//! an in-memory bus instead of the multicast group, on tokio's paused
//! clock (`#[tokio::test(start_paused = true)]`): a minute of cluster life
//! takes milliseconds, nothing depends on the host's network, and the
//! same seed always plays out the same way. The bus can unplug a box, cut
//! links between groups of boxes, inject forged or replayed datagrams, and
//! keeps a log of everything sent.

use std::{
    collections::{HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use tokio::{task::JoinHandle, time::Instant};
use uuid::Uuid;
use xdebbie_core::TokenSet;

use super::{
    engine::{DiscoveryConfig, DiscoveryEngine, Host, Parts},
    messages::NodeCapacity,
    multicast::Transport,
};
use crate::{election::state::ElectionState, leader::Leadership, token::Tokens};

/// Wall-clock time (Unix seconds) at the start of every simulation.
pub const EPOCH: u64 = 1_763_400_000;

/// One datagram as it went out.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Since the LAN was made.
    pub at: Duration,
    pub from: usize,
    pub src: SocketAddr,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
struct Bus {
    inbox: Vec<VecDeque<(Vec<u8>, SocketAddr)>>,
    addr: Vec<SocketAddr>,
    unplugged: Vec<bool>,
    /// Links that carry nothing, as unordered pairs.
    cut: HashSet<(usize, usize)>,
    log: Vec<Frame>,
}

impl Bus {
    fn linked(&self, a: usize, b: usize) -> bool {
        a == b
            || !(self.unplugged[a] || self.unplugged[b] || self.cut.contains(&(a.min(b), a.max(b))))
    }
}

/// The LAN: one shared segment, every box hears every other (and itself,
/// as multicast loopback does) unless unplugged or cut off.
#[derive(Clone)]
pub struct Lan {
    bus: Arc<Mutex<Bus>>,
    origin: Instant,
}

impl Default for Lan {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static ORIGIN: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Simulated time in log lines.
struct SimTime;

impl tracing_subscriber::fmt::time::FormatTime for SimTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        match ORIGIN.with(|o| o.get()) {
            Some(o) => write!(w, "{:>7.1}s", o.elapsed().as_secs_f64()),
            None => write!(w, "      ?"),
        }
    }
}

/// `XDEBBIE_SIM_TRACE=1` (or a filter such as `debug`): print every
/// engine's log, stamped with simulated time and the box's name.
fn trace() {
    let Ok(f) = std::env::var("XDEBBIE_SIM_TRACE") else {
        return;
    };
    let f = if f == "1" { "info".to_string() } else { f };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(f))
        .with_timer(SimTime)
        .with_test_writer()
        .try_init();
}

impl Lan {
    pub fn new() -> Self {
        trace();
        ORIGIN.with(|o| o.set(Some(Instant::now())));
        Self {
            bus: Arc::default(),
            origin: Instant::now(),
        }
    }

    /// Time since the LAN was made.
    pub fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    /// The wall clock of a box with no skew.
    pub fn wall(&self) -> u64 {
        EPOCH + self.now().as_secs()
    }

    fn attach(&self) -> usize {
        let mut b = self.bus.lock().unwrap();
        let i = b.inbox.len();
        b.inbox.push(VecDeque::new());
        b.addr
            .push(SocketAddr::from(([192, 0, 2, 10 + i as u8], 23442)));
        b.unplugged.push(false);
        i
    }

    /// Start a box running `spec`.
    pub fn spawn(&self, spec: Spec) -> SimBox {
        let idx = self.attach();
        let ip = self.bus.lock().unwrap().addr[idx].ip();
        let host = Arc::new(SimHost {
            uptime: spec.uptime,
            grows: spec.uptime_grows,
            started: Instant::now(),
            cpus: spec.cpus,
            skew: AtomicI64::new(spec.skew_secs),
            origin: self.origin,
        });
        let api_url = format!("https://{ip}:23443");
        let config = DiscoveryConfig {
            multicast_addr: "239.0.0.42:23442".into(),
            bind_addr: None,
            hostname: spec.name.clone(),
            // A name nobody resolves, so peers must swap in the address
            // (reach.rs), as on a fresh LAN without DNS.
            api_url: format!("https://{}.invalid:23443", spec.name),
            addrs: vec![ip],
            vm_count: Arc::new(AtomicUsize::new(spec.vm_count)),
            enable_discovery: spec.enabled,
            tokens: spec.tokens.map(|t| Arc::new(Tokens::in_memory(t))),
            accept_unsigned: spec.accept_unsigned,
        };
        let engine = Arc::new(DiscoveryEngine::with_parts(
            config,
            Parts {
                node_id: spec.id,
                transport: Box::new(Port {
                    bus: self.bus.clone(),
                    idx,
                    origin: self.origin,
                }),
                host: host.clone(),
            },
        ));
        let leadership = Leadership::elected(&engine, spec.name.clone(), api_url.clone());
        let run = engine.clone();
        let span = tracing::info_span!("box", name = %spec.name);
        let task = tokio::spawn(tracing::Instrument::instrument(
            async move {
                let _ = run.run().await;
            },
            span,
        ));
        SimBox {
            idx,
            name: spec.name,
            api_url,
            engine,
            leadership,
            host,
            task,
        }
    }

    /// Pull a box's cable: it hears and is heard by nobody.
    pub fn unplug(&self, b: &SimBox) {
        self.bus.lock().unwrap().unplugged[b.idx] = true;
    }

    pub fn plug(&self, b: &SimBox) {
        let mut bus = self.bus.lock().unwrap();
        bus.unplugged[b.idx] = false;
    }

    /// Cut every link between boxes in different groups.
    pub fn partition(&self, groups: &[&[&SimBox]]) {
        let mut bus = self.bus.lock().unwrap();
        for (gi, g) in groups.iter().enumerate() {
            for h in groups.iter().skip(gi + 1) {
                for a in g.iter() {
                    for b in h.iter() {
                        bus.cut.insert((a.idx.min(b.idx), a.idx.max(b.idx)));
                    }
                }
            }
        }
    }

    /// Restore every cut link (unplugged boxes stay unplugged).
    pub fn heal(&self) {
        self.bus.lock().unwrap().cut.clear();
    }

    /// Deliver `bytes` to `to` as if sent from `src`: an attacker on the
    /// segment, or a replay.
    pub fn inject(&self, to: &SimBox, bytes: Vec<u8>, src: SocketAddr) {
        self.bus.lock().unwrap().inbox[to.idx].push_back((bytes, src));
    }

    /// Everything sent so far.
    pub fn log(&self) -> Vec<Frame> {
        self.bus.lock().unwrap().log.clone()
    }
}

struct Port {
    bus: Arc<Mutex<Bus>>,
    idx: usize,
    origin: Instant,
}

impl Transport for Port {
    fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut bus = self.bus.lock().unwrap();
        let src = bus.addr[self.idx];
        let at = self.origin.elapsed();
        bus.log.push(Frame {
            at,
            from: self.idx,
            src,
            bytes: bytes.to_vec(),
        });
        for j in 0..bus.inbox.len() {
            if bus.linked(self.idx, j) {
                bus.inbox[j].push_back((bytes.to_vec(), src));
            }
        }
        Ok(())
    }

    fn recv(&self) -> Result<Option<(Vec<u8>, SocketAddr)>> {
        Ok(self.bus.lock().unwrap().inbox[self.idx].pop_front())
    }
}

struct SimHost {
    uptime: Duration,
    grows: bool,
    started: Instant,
    cpus: u32,
    skew: AtomicI64,
    origin: Instant,
}

impl Host for SimHost {
    fn uptime(&self) -> Duration {
        if self.grows {
            self.uptime + self.started.elapsed()
        } else {
            self.uptime
        }
    }

    fn capacity(&self, vm_count: usize) -> NodeCapacity {
        NodeCapacity {
            total_cpus: self.cpus,
            total_memory_mib: 65536,
            available_cpus: self.cpus as f32,
            available_memory_mib: 65536,
            vm_count,
            control_plane_eligible: true,
        }
    }

    fn wall_secs(&self) -> u64 {
        (EPOCH as i64 + self.origin.elapsed().as_secs() as i64 + self.skew.load(Ordering::SeqCst))
            as u64
    }

    fn resolves(&self, _: &str) -> bool {
        false
    }
}

/// How a simulated box is built.
#[derive(Clone)]
pub struct Spec {
    pub name: String,
    pub id: Uuid,
    pub vm_count: usize,
    pub cpus: u32,
    /// Uptime when the box starts.
    pub uptime: Duration,
    /// Does uptime advance with the clock? Off by default so scores hold
    /// still and only what a test varies decides an election.
    pub uptime_grows: bool,
    pub tokens: Option<TokenSet>,
    pub accept_unsigned: bool,
    pub enabled: bool,
    pub skew_secs: i64,
}

/// The cluster token every box shares unless a test says otherwise.
pub const TOKEN: &str = "cluster-token";

impl Spec {
    /// A box that differs from its siblings only in name and `rank`, which
    /// orders the UUID tiebreak (higher rank, higher tiebreak).
    pub fn new(name: &str, rank: u8) -> Self {
        Self {
            name: name.into(),
            id: id(rank),
            vm_count: 0,
            cpus: 8,
            uptime: Duration::from_secs(48 * 3600),
            uptime_grows: false,
            tokens: Some(TokenSet {
                current: TOKEN.into(),
                previous: None,
            }),
            accept_unsigned: false,
            enabled: true,
            skew_secs: 0,
        }
    }

    pub fn vms(mut self, n: usize) -> Self {
        self.vm_count = n;
        self
    }

    pub fn cpus(mut self, n: u32) -> Self {
        self.cpus = n;
        self
    }

    pub fn token(mut self, current: &str, previous: Option<&str>) -> Self {
        self.tokens = Some(TokenSet {
            current: current.into(),
            previous: previous.map(Into::into),
        });
        self
    }

    pub fn no_token(mut self) -> Self {
        self.tokens = None;
        self
    }

    pub fn accept_unsigned(mut self) -> Self {
        self.accept_unsigned = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn skew(mut self, secs: i64) -> Self {
        self.skew_secs = secs;
        self
    }

    /// Same box, new process: a fresh node id, as a restarted daemon gets.
    pub fn restarted(mut self) -> Self {
        let mut b = *self.id.as_bytes();
        b[15] ^= 0xff;
        self.id = Uuid::from_bytes(b);
        self
    }
}

/// A node id whose tiebreak (first 8 bytes, engine scoring) grows with
/// `rank`.
pub fn id(rank: u8) -> Uuid {
    let mut b = [0u8; 16];
    b[0] = rank;
    b[15] = rank;
    Uuid::from_bytes(b)
}

/// A running simulated box.
pub struct SimBox {
    pub idx: usize,
    pub name: String,
    /// The URL peers reach it at (what they rewrite its announced name to).
    pub api_url: String,
    pub engine: Arc<DiscoveryEngine>,
    pub leadership: Leadership,
    host: Arc<SimHost>,
    task: JoinHandle<()>,
}

impl SimBox {
    pub async fn state(&self) -> ElectionState {
        self.engine.election().read().await.state()
    }

    pub async fn is_leader(&self) -> bool {
        self.state().await == ElectionState::Leader
    }

    /// Who this box would forward a cluster request to.
    pub async fn leader_url(&self) -> Option<String> {
        self.leadership.leader().await.map(|l| l.api_url)
    }

    /// Names of the peers this box knows.
    pub async fn peer_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .leadership
            .peers()
            .await
            .into_iter()
            .map(|p| p.hostname)
            .collect();
        v.sort();
        v
    }

    pub fn set_skew(&self, secs: i64) {
        self.host.skew.store(secs, Ordering::SeqCst);
    }

    /// The daemon dies: it stops sending and hearing at once.
    pub fn kill(&self) {
        self.task.abort();
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn src(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10 + self.idx as u8)),
            23442,
        )
    }
}

impl Drop for SimBox {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Let simulated time pass.
pub async fn run_for(d: Duration) {
    tokio::time::sleep(d).await;
}

pub fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// Names of the running boxes that consider themselves leader.
pub async fn leaders(boxes: &[&SimBox]) -> Vec<String> {
    let mut v = vec![];
    for b in boxes {
        if b.is_running() && b.is_leader().await {
            v.push(b.name.clone());
        }
    }
    v
}

/// Is the cluster settled: exactly one running leader, and every other
/// running box a follower pointing at it?
pub async fn settled(boxes: &[&SimBox]) -> Option<String> {
    let l = leaders(boxes).await;
    let [name] = l.as_slice() else {
        return None;
    };
    let leader = boxes.iter().find(|b| &b.name == name)?;
    for b in boxes.iter().filter(|b| b.is_running() && &b.name != name) {
        if b.state().await != ElectionState::Follower
            || b.leader_url().await.as_deref() != Some(&leader.api_url)
        {
            return None;
        }
    }
    Some(name.clone())
}

/// Step time in 100 ms ticks until [`settled`]; panics after `max`.
/// Returns the leader and how long it took.
pub async fn settle(boxes: &[&SimBox], max: Duration) -> (String, Duration) {
    let t0 = Instant::now();
    loop {
        if let Some(l) = settled(boxes).await {
            return (l, t0.elapsed());
        }
        if t0.elapsed() > max {
            let mut states = vec![];
            for b in boxes {
                states.push(format!(
                    "{}: {:?} -> {:?} peers {:?}",
                    b.name,
                    b.state().await,
                    b.leader_url().await,
                    b.peer_names().await
                ));
            }
            panic!("not settled within {max:?}: {states:#?}");
        }
        run_for(Duration::from_millis(100)).await;
    }
}

/// Watch for `d` in 100 ms ticks; returns every change of the leader set
/// as (time since the watch started, leaders).
pub async fn watch(boxes: &[&SimBox], d: Duration) -> Vec<(Duration, Vec<String>)> {
    let t0 = Instant::now();
    let mut out: Vec<(Duration, Vec<String>)> = vec![];
    while t0.elapsed() <= d {
        let l = leaders(boxes).await;
        if out.last().is_none_or(|(_, prev)| *prev != l) {
            out.push((t0.elapsed(), l));
        }
        run_for(Duration::from_millis(100)).await;
    }
    out
}
