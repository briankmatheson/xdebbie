// SPDX-License-Identifier: GPL-3.0-or-later
//! Geneve mesh between hypervisors (requirement X12).
//!
//! `xdebbied` keeps one Geneve tunnel port per peer enslaved to the overlay
//! bridge, so VMs on `br-int` share one L2 domain across hosts. This module
//! owns the desired state, reconciles the kernel toward it, probes peers,
//! and exposes it all over the API.
//!
//! # Flock contract (for `flockd`; not built here)
//!
//! - `flockd` on each box calls its **local** `xdebbied`: `PUT /v1/mesh`
//!   with `If-Match: <generation>`, sending the full peer list built from
//!   the Ready `FlockBox` resources. Each peer is `remote = 10.22.<box>.1`
//!   with `probe` left out (derived).
//! - Enroll order: WireGuard peer up → `PUT` with the new peer.
//! - Revoke order: `PUT` without the peer → wait until `GET` shows
//!   `observed_generation == generation` → remove the WireGuard peer.
//! - `FlockBox.status.conditions[MeshReady]` is computed by Flock from
//!   every other box's `GET /v1/mesh`. The mesh only reports its own view.
//!
//! # Ownership
//!
//! A link is mesh-owned only if its name is `gnv-b<P>` **and** its ifalias
//! is `xdebbie-mesh/v1`. Nothing else is ever modified or deleted. The only
//! nftables object touched is `table inet xdebbie_mesh`.

pub mod firewall;
pub mod kernel;
pub mod metrics;
pub mod model;
pub mod plan;
pub mod probe;
pub mod validate;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify, RwLock, mpsc};
use tracing::{debug, info, warn};

pub use kernel::{FakeNetOps, IpCliNetOps, NetOps};
pub use model::*;
pub use validate::FieldError;

use kernel::Observed;
use probe::{Hysteresis, IcmpClient, ProbeEvent, ProbeParams};

/// Default reconcile period when no notify arrives.
pub const RECONCILE_PERIOD: Duration = Duration::from_secs(5);
const STATE_FILE: &str = "mesh.json";
const NFT_FILE: &str = "mesh.nft";

#[derive(Debug, Clone)]
struct Desired {
    spec: MeshSpec,
    source: Source,
    generation: u64,
}

#[derive(Debug)]
struct PeerTrack {
    hyst: Hysteresis,
    last_change: DateTime<Utc>,
    rtt_ms: Option<f64>,
    reason: Option<String>,
    route_dev: Option<String>,
    failures: u64,
}

impl PeerTrack {
    fn new(spec: &ProbeSpec) -> Self {
        Self {
            hyst: Hysteresis::new(spec.up_after, spec.down_after),
            last_change: Utc::now(),
            rtt_ms: None,
            reason: None,
            route_dev: None,
            failures: 0,
        }
    }
    fn transition(&mut self, changed: Option<PeerState>) {
        if changed.is_some() {
            self.last_change = Utc::now();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProbeKey {
    target: std::net::Ipv4Addr,
    interval_ms: u64,
    timeout_ms: u64,
}

/// What libvirt needs from the mesh: the overlay bridge and the MTU every
/// NIC on it should get. `None` while the mesh is disabled or blocked.
#[derive(Debug, Default)]
pub struct Overlay {
    inner: StdRwLock<Option<(String, u16)>>,
}

impl Overlay {
    /// MTU for a NIC attached to `bridge`, if the mesh manages that bridge.
    pub fn mtu_for(&self, bridge: &str) -> Option<u16> {
        let g = self.inner.read().unwrap();
        match &*g {
            Some((b, m)) if b == bridge => Some(*m),
            _ => None,
        }
    }
    fn set(&self, v: Option<(String, u16)>) {
        *self.inner.write().unwrap() = v;
    }
}

pub enum PutError {
    Validation(Vec<FieldError>),
    Conflict { current: u64 },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for PutError {
    fn from(e: anyhow::Error) -> Self {
        PutError::Internal(e)
    }
}

pub struct MeshManager {
    ops: Arc<dyn NetOps>,
    state_dir: PathBuf,
    config_spec: MeshSpec,
    desired: RwLock<Desired>,
    status: RwLock<MeshStatus>,
    notify: Notify,
    reconcile_lock: Mutex<()>,
    tracks: StdMutex<HashMap<u8, PeerTrack>>,
    probers: StdMutex<HashMap<u8, (ProbeKey, tokio::task::JoinHandle<()>)>>,
    probe_tx: mpsc::Sender<ProbeEvent>,
    probe_rx: StdMutex<Option<mpsc::Receiver<ProbeEvent>>>,
    icmp: StdMutex<Option<IcmpClient>>,
    last_nft: StdMutex<Option<String>>,
    tap_owners: StdMutex<HashMap<String, String>>,
    pub overlay: Arc<Overlay>,
    /// Set by tests to skip spawning ICMP probers.
    probes_enabled: bool,
}

impl MeshManager {
    /// `config` is the `[mesh]` TOML section (or `None` for all defaults).
    /// If `<state_dir>/mesh.json` exists it wins (source = api).
    pub fn new(ops: Arc<dyn NetOps>, state_dir: PathBuf, config: Option<&MeshConfig>) -> Result<Arc<Self>> {
        let config_spec = config.cloned().unwrap_or_default().to_spec();
        // TOML validation failures stop the daemon (section 3.2).
        let env = env_for(&*ops, &config_spec);
        validate::validate_or_err(&config_spec, &env)?;

        let path = state_dir.join(STATE_FILE);
        let desired = match std::fs::read(&path) {
            Ok(b) => {
                let spec: MeshSpec = serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?;
                let spec = merge_defaults(spec, &config_spec);
                if let Err(e) = validate::validate_or_err(&spec, &env_for(&*ops, &spec)) {
                    warn!("{} is invalid ({e:#}); ignoring it and using [mesh] config", path.display());
                    Desired { spec: config_spec.clone(), source: Source::Config, generation: 1 }
                } else {
                    info!("mesh desired state from {}", path.display());
                    Desired { spec, source: Source::Api, generation: 1 }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Desired { spec: config_spec.clone(), source: Source::Config, generation: 1 }
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let last_nft = std::fs::read_to_string(state_dir.join(NFT_FILE)).ok();
        let (tx, rx) = mpsc::channel(256);
        let status = MeshStatus {
            source: Some(desired.source),
            generation: desired.generation,
            ..Default::default()
        };
        Ok(Arc::new(Self {
            ops,
            state_dir,
            config_spec,
            desired: RwLock::new(desired),
            status: RwLock::new(status),
            notify: Notify::new(),
            reconcile_lock: Mutex::new(()),
            tracks: StdMutex::new(HashMap::new()),
            probers: StdMutex::new(HashMap::new()),
            probe_tx: tx,
            probe_rx: StdMutex::new(Some(rx)),
            icmp: StdMutex::new(None),
            last_nft: StdMutex::new(last_nft),
            tap_owners: StdMutex::new(HashMap::new()),
            overlay: Arc::new(Overlay::default()),
            probes_enabled: true,
        }))
    }

    #[cfg(test)]
    pub fn new_for_test(ops: Arc<dyn NetOps>, state_dir: PathBuf, config: Option<&MeshConfig>) -> Result<Arc<Self>> {
        let m = Self::new(ops, state_dir, config)?;
        // Safe: no other Arc yet.
        let mut m = Arc::try_unwrap(m).ok().expect("unique");
        m.probes_enabled = false;
        Ok(Arc::new(m))
    }

    // ---------- API surface ----------

    pub async fn get(&self) -> (MeshSpec, MeshStatus) {
        let d = self.desired.read().await;
        let s = self.status.read().await;
        (d.spec.clone(), s.clone())
    }

    pub async fn peer(&self, box_id: u8) -> Option<PeerStatus> {
        self.status.read().await.peers.iter().find(|p| p.box_id == box_id).cloned()
    }

    /// Declarative replace. Same body twice is a no-op (no generation bump).
    pub async fn put(&self, spec: MeshSpec, if_match: Option<u64>) -> Result<(MeshSpec, MeshStatus), PutError> {
        let spec = merge_defaults(spec, &self.config_spec);
        let env = env_for(&*self.ops, &spec);
        let errs = validate::validate(&spec, &env);
        if !errs.is_empty() {
            return Err(PutError::Validation(errs));
        }
        {
            let mut d = self.desired.write().await;
            if let Some(g) = if_match
                && g != d.generation
            {
                return Err(PutError::Conflict { current: d.generation });
            }
            if d.spec == spec && d.source == Source::Api {
                debug!("PUT /v1/mesh: unchanged");
                drop(d);
                let (s, st) = self.get().await;
                return Ok((s, st));
            }
            persist(&self.state_dir.join(STATE_FILE), &spec).map_err(PutError::Internal)?;
            d.spec = spec;
            d.source = Source::Api;
            d.generation += 1;
            let mut st = self.status.write().await;
            st.generation = d.generation;
            st.source = Some(Source::Api);
            info!(generation = d.generation, "mesh desired state updated via API");
        }
        self.reconcile_once(false).await;
        Ok(self.get().await)
    }

    /// Remove `mesh.json`, tear everything down, fall back to the TOML
    /// section. Returns the status and whether config re-enabled the mesh.
    pub async fn delete(&self) -> Result<(MeshStatus, bool)> {
        {
            let mut d = self.desired.write().await;
            let p = self.state_dir.join(STATE_FILE);
            match std::fs::remove_file(&p) {
                Ok(()) => info!("removed {}", p.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", p.display())),
            }
            d.spec = self.config_spec.clone();
            d.source = Source::Config;
            d.generation += 1;
            let mut st = self.status.write().await;
            st.generation = d.generation;
            st.source = Some(Source::Config);
        }
        // Tear down first, then (if config says so) build back from config.
        self.reconcile_once(true).await;
        let comes_back = self.config_spec.enabled;
        if comes_back {
            info!("[mesh] in xdebbied.toml has enabled = true; mesh comes back from config");
            self.reconcile_once(false).await;
        }
        let (_, st) = self.get().await;
        Ok((st, comes_back))
    }

    pub fn trigger(&self) {
        self.notify.notify_one();
    }

    /// Tap → VM name mapping, supplied by whoever talks to libvirt, so
    /// `vms_mtu_mismatch` can name VMs rather than `vnetN`.
    pub fn set_tap_owners(&self, map: HashMap<String, String>) {
        *self.tap_owners.lock().unwrap() = map;
    }

    /// Bridge ports (non-mesh) whose MTU differs from the mesh MTU.
    pub async fn mismatched_ports(&self) -> Vec<String> {
        self.status.read().await.vms_mtu_mismatch.clone()
    }

    // ---------- main loop ----------

    pub async fn run(self: Arc<Self>) {
        let mut rx = self.probe_rx.lock().unwrap().take().expect("run() called twice");
        self.reconcile_once(false).await;
        let mut tick = tokio::time::interval(RECONCILE_PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => self.reconcile_once(false).await,
                _ = self.notify.notified() => self.reconcile_once(false).await,
                Some(ev) = rx.recv() => self.on_probe(ev).await,
            }
        }
    }

    async fn on_probe(&self, ev: ProbeEvent) {
        let changed = {
            let mut tracks = self.tracks.lock().unwrap();
            let Some(t) = tracks.get_mut(&ev.box_id) else { return };
            if ev.ok {
                t.rtt_ms = ev.rtt_ms;
            } else {
                t.failures += 1;
            }
            let c = t.hyst.observe(ev.ok);
            t.transition(c);
            c
        };
        if let Some(state) = changed {
            info!(peer = ev.box_id, state = state.as_str(), "mesh peer state changed");
        }
        // Refresh the peer rows and Ready/Degraded without touching the kernel.
        let mut st = self.status.write().await;
        self.refresh_peer_rows(&mut st, None);
    }

    // ---------- reconcile ----------

    /// One pass (section 6.2). `teardown` forces removal regardless of spec.
    pub async fn reconcile_once(&self, teardown: bool) {
        let _g = self.reconcile_lock.lock().await;
        let d = self.desired.read().await.clone();
        let mut st = self.status.read().await.clone();
        st.generation = d.generation;
        st.source = Some(d.source);

        let result = if teardown || !d.spec.enabled {
            self.do_teardown(&d, &mut st)
        } else {
            self.do_reconcile(&d, &mut st)
        };
        st.metrics.inc_reconcile(result);
        st.metrics.generation = st.generation;
        st.metrics.observed_generation = st.observed_generation;
        st.metrics.mtu_bytes = st.mtu;
        *self.status.write().await = st;
    }

    fn do_teardown(&self, d: &Desired, st: &mut MeshStatus) -> &'static str {
        let bridge = d.spec.bridge();
        let observed = match self.ops.observe(&bridge) {
            Ok(o) => o,
            Err(e) => {
                warn!("mesh: observing kernel state failed: {e:#}");
                return "error";
            }
        };
        let actions = plan::teardown(&observed);
        let mut ok = true;
        for a in &actions {
            info!(action = a.kind(), dev = a.dev(), "mesh teardown");
            st.metrics.inc_action(a.kind());
            if let Err(e) = self.ops.apply(a) {
                warn!(dev = a.dev(), "mesh teardown action failed: {e:#}");
                ok = false;
            }
        }
        if let Err(e) = self.ops.nft_delete_table() {
            warn!("mesh: deleting nft table failed: {e:#}");
            ok = false;
        }
        *self.last_nft.lock().unwrap() = None;
        let _ = std::fs::remove_file(self.state_dir.join(NFT_FILE));
        self.stop_all_probers();
        self.tracks.lock().unwrap().clear();
        self.overlay.set(None);
        st.peers.clear();
        st.vms_mtu_mismatch.clear();
        st.mtu = 0;
        st.metrics.retain_peers(&[]);
        for c in [
            ConditionType::GeneveModuleMissing,
            ConditionType::UnderlayMissing,
            ConditionType::BridgeMissing,
            ConditionType::BridgeStpEnabled,
            ConditionType::BridgeMacMismatch,
            ConditionType::NameConflict,
            ConditionType::Degraded,
        ] {
            set_condition(&mut st.conditions, c, false, None, None);
        }
        set_condition(&mut st.conditions, ConditionType::Ready, false, Some("Disabled"), None);
        if ok {
            st.observed_generation = st.generation;
            if actions.is_empty() { "noop" } else { "ok" }
        } else {
            "error"
        }
    }

    fn do_reconcile(&self, d: &Desired, st: &mut MeshStatus) -> &'static str {
        let spec = &d.spec;
        let bridge = spec.bridge();

        // 3. preflight
        let links = match self.ops.list_links() {
            Ok(l) => l,
            Err(e) => {
                warn!("mesh: listing links failed: {e:#}");
                return "error";
            }
        };
        let module_ok = self.ops.geneve_module_present();
        let bridge_link = links.iter().find(|l| l.name == bridge && l.is_bridge());
        let underlay_link = links.iter().find(|l| l.name == spec.underlay);
        set_condition(&mut st.conditions, ConditionType::GeneveModuleMissing, !module_ok, Some("NoSysModule"),
            (!module_ok).then(|| "/sys/module/geneve does not exist; the .deb ships /etc/modules-load.d/xdebbie-mesh.conf".to_string()));
        set_condition(&mut st.conditions, ConditionType::BridgeMissing, bridge_link.is_none(), Some("NotFound"),
            bridge_link.is_none().then(|| format!("{bridge} does not exist or is not a bridge")));
        set_condition(&mut st.conditions, ConditionType::UnderlayMissing, underlay_link.is_none(), Some("NotFound"),
            underlay_link.is_none().then(|| format!("underlay interface {} does not exist", spec.underlay)));
        let (Some(bridge_link), Some(underlay_link), true) = (bridge_link, underlay_link, module_ok) else {
            set_condition(&mut st.conditions, ConditionType::Ready, false, Some("Blocked"), Some("a blocking condition is set; kernel left unchanged".into()));
            self.overlay.set(None);
            debug!("mesh: blocked by preflight; no changes");
            return "error";
        };

        // 4. mtu
        let computed = auto_mtu(underlay_link.mtu);
        let mtu = match spec.mtu {
            MtuSpec::Auto => computed,
            MtuSpec::Fixed(m) if m <= computed => m,
            MtuSpec::Fixed(m) => {
                warn!("mesh: fixed mtu {m} exceeds computed {computed} (underlay mtu changed?); using {computed}");
                computed
            }
        };
        st.mtu = mtu;
        self.overlay.set(Some((bridge.clone(), mtu)));

        // 5. observe
        let observed = match self.ops.observe(&bridge) {
            Ok(o) => o,
            Err(e) => {
                warn!("mesh: observing kernel state failed: {e:#}");
                return "error";
            }
        };

        // 6. underlay route check
        let mut route_bad = BTreeSet::new();
        let mut route_dev: BTreeMap<u8, Option<String>> = BTreeMap::new();
        for p in &spec.peers {
            match self.ops.route_get(p.remote) {
                Ok(r) => {
                    if r.dev != spec.underlay && !spec.allow_plain_underlay {
                        route_bad.insert(p.box_id);
                    }
                    route_dev.insert(p.box_id, Some(r.dev));
                }
                Err(e) => {
                    debug!(peer = p.box_id, "mesh: route lookup for {} failed: {e:#}", p.remote);
                    if !spec.allow_plain_underlay {
                        route_bad.insert(p.box_id);
                    }
                    route_dev.insert(p.box_id, None);
                }
            }
        }

        // 7. plan
        let plan = plan::diff(plan::PlanInput { spec, observed: &observed, mtu, route_bad: &route_bad });

        // 8. apply
        let mut failed: BTreeMap<u8, String> = BTreeMap::new();
        let mut all_ok = true;
        for a in &plan.actions {
            let peer = parse_tunnel_name(a.dev());
            match self.ops.apply(a) {
                Ok(()) => {
                    info!(action = a.kind(), dev = a.dev(), peer = ?peer, "mesh");
                    st.metrics.inc_action(a.kind());
                }
                Err(e) => {
                    all_ok = false;
                    warn!(action = a.kind(), dev = a.dev(), "mesh action failed: {e:#}");
                    if let Some(b) = peer {
                        failed.entry(b).or_insert_with(|| format!("{}: {e}", a.kind()));
                    }
                }
            }
        }

        // 9. firewall
        if spec.firewall {
            let rendered = firewall::render(spec);
            let unchanged = self.last_nft.lock().unwrap().as_deref() == Some(rendered.as_str()) && self.ops.nft_table_exists();
            if !unchanged {
                match self.ops.nft_replace_table(&rendered) {
                    Ok(()) => {
                        info!("mesh: applied nft table {NFT_TABLE}");
                        *self.last_nft.lock().unwrap() = Some(rendered.clone());
                        let _ = std::fs::write(self.state_dir.join(NFT_FILE), &rendered);
                    }
                    Err(e) => {
                        all_ok = false;
                        warn!("mesh: applying nft table failed: {e:#}");
                    }
                }
            }
        } else if self.last_nft.lock().unwrap().is_some() || self.ops.nft_table_exists() {
            match self.ops.nft_delete_table() {
                Ok(()) => {
                    *self.last_nft.lock().unwrap() = None;
                    let _ = std::fs::remove_file(self.state_dir.join(NFT_FILE));
                }
                Err(e) => warn!("mesh: deleting nft table failed: {e:#}"),
            }
        }

        // 10. bridge checks (warnings only)
        let stp = matches!(&bridge_link.kind, kernel::LinkKind::Bridge(b) if b.stp_enabled);
        set_condition(&mut st.conditions, ConditionType::BridgeStpEnabled, stp, Some("StpOn"),
            stp.then(|| format!("STP is enabled on {bridge}; the mesh relies on isolated ports, not STP")));
        let want_mac = bridge_mac(spec.box_id);
        let mac_bad = spec.fdb == FdbMode::Derived
            && bridge_link.address.as_deref().map(|a| !a.eq_ignore_ascii_case(&want_mac)).unwrap_or(true);
        set_condition(&mut st.conditions, ConditionType::BridgeMacMismatch, mac_bad, Some("DerivedPlan"),
            mac_bad.then(|| format!("{bridge} has MAC {}, derived plan expects {want_mac}", bridge_link.address.clone().unwrap_or_default())));

        // VM NIC MTU mismatches: non-mesh ports on the bridge.
        let owners = self.tap_owners.lock().unwrap().clone();
        let mut mism: Vec<String> = observed
            .ports
            .iter()
            .filter(|p| p.master == bridge)
            .filter_map(|p| observed.link(&p.name))
            .filter(|l| !l.mesh_owned() && l.mtu != mtu as u32)
            .map(|l| match owners.get(&l.name) {
                Some(vm) => format!("{vm} ({})", l.name),
                None => l.name.clone(),
            })
            .collect();
        mism.sort();
        st.vms_mtu_mismatch = mism;

        // Peers: Error bookkeeping, probers.
        let excluded: BTreeMap<u8, &'static str> = plan.excluded.iter().map(|e| (e.box_id, e.reason)).collect();
        let name_conflict = plan.excluded.iter().any(|e| e.reason == "NameConflict");
        set_condition(&mut st.conditions, ConditionType::NameConflict, name_conflict, Some("ForeignLink"),
            name_conflict.then(|| "a non-mesh interface uses a gnv-b<P> name".to_string()));
        {
            let mut tracks = self.tracks.lock().unwrap();
            let live: Vec<u8> = spec.peers.iter().map(|p| p.box_id).collect();
            tracks.retain(|b, _| live.contains(b));
            for p in &spec.peers {
                let t = tracks.entry(p.box_id).or_insert_with(|| PeerTrack::new(&spec.probe));
                if t.hyst.up_after != spec.probe.up_after.max(1) || t.hyst.down_after != spec.probe.down_after.max(1) {
                    *t = PeerTrack::new(&spec.probe);
                }
                t.route_dev = route_dev.get(&p.box_id).cloned().flatten();
                let err_reason = excluded.get(&p.box_id).map(|r| r.to_string()).or_else(|| failed.get(&p.box_id).cloned());
                match err_reason {
                    Some(r) => {
                        let c = t.hyst.set_error();
                        t.transition(c);
                        t.reason = Some(r);
                    }
                    None => {
                        let c = t.hyst.clear_error();
                        t.transition(c);
                        t.reason = None;
                    }
                }
            }
        }
        self.sync_probers(spec, &excluded);

        // 11/12. status
        let observed_after = if plan.actions.is_empty() {
            observed
        } else {
            self.ops.observe(&bridge).unwrap_or(observed)
        };
        if all_ok {
            st.observed_generation = st.generation;
        }
        self.refresh_peer_rows(st, Some(&observed_after));
        st.metrics.retain_peers(&spec.peers.iter().map(|p| p.box_id).collect::<Vec<_>>());
        for p in &spec.peers {
            if let Some((rx, tx)) = self.ops.link_stats(&tunnel_name(p.box_id)) {
                let m = st.metrics.peer(p.box_id);
                m.tunnel_rx_bytes_total = rx;
                m.tunnel_tx_bytes_total = tx;
            }
        }

        if plan.actions.is_empty() && all_ok {
            debug!("mesh: reconcile no-op");
            "noop"
        } else if all_ok {
            "ok"
        } else {
            "error"
        }
    }

    /// Rebuild `status.peers` and Ready/Degraded from tracks (+ kernel view).
    fn refresh_peer_rows(&self, st: &mut MeshStatus, observed: Option<&Observed>) {
        let spec = match self.desired.try_read() {
            Ok(d) => d.spec.clone(),
            Err(_) => return,
        };
        if !spec.enabled {
            return;
        }
        let tracks = self.tracks.lock().unwrap();
        let mut rows = Vec::with_capacity(spec.peers.len());
        let mut any_bad = false;
        let mut all_up = !spec.peers.is_empty() || true;
        for p in &spec.peers {
            let dev = tunnel_name(p.box_id);
            let (state, reason, rtt, last_change, route_dev, failures) = match tracks.get(&p.box_id) {
                Some(t) => (t.hyst.state, t.reason.clone(), t.rtt_ms, t.last_change, t.route_dev.clone(), t.failures),
                None => (PeerState::Pending, None, None, Utc::now(), None, 0),
            };
            if matches!(state, PeerState::Down | PeerState::Error) {
                any_bad = true;
            }
            if state != PeerState::Up {
                all_up = false;
            }
            let prev = st.peers.iter().find(|r| r.box_id == p.box_id);
            let (ifindex, fdb_entries) = match observed {
                Some(o) => (
                    o.link(&dev).map(|l| l.ifindex),
                    o.fdb.iter().filter(|e| e.is_static && e.dev == dev).count() as u32,
                ),
                None => (prev.and_then(|r| r.ifindex), prev.map(|r| r.fdb_entries).unwrap_or(0)),
            };
            let m = st.metrics.peer(p.box_id);
            m.probe_failures_total = failures;
            st.metrics.set_peer_state(p.box_id, state, rtt);
            rows.push(PeerStatus {
                box_id: p.box_id,
                remote: p.remote,
                probe: spec.probe_addr(p),
                device: dev,
                ifindex,
                state,
                reason,
                rtt_ms: rtt,
                last_change,
                fdb_entries,
                route_dev: if spec.allow_plain_underlay { route_dev } else { None },
            });
        }
        st.peers = rows;
        set_condition(&mut st.conditions, ConditionType::Degraded, any_bad, Some("PeerDownOrError"),
            any_bad.then(|| "at least one peer is Down or Error".to_string()));
        let blocked = st.conditions.iter().any(|c| c.status && c.kind.is_blocking());
        let ready = all_up && st.observed_generation == st.generation && !blocked;
        let reason = if blocked { "Blocked" } else if st.observed_generation != st.generation { "Reconciling" } else if !all_up { "PeersNotUp" } else { "AllPeersUp" };
        set_condition(&mut st.conditions, ConditionType::Ready, ready, Some(reason), None);
    }

    // ---------- probers ----------

    fn sync_probers(&self, spec: &MeshSpec, excluded: &BTreeMap<u8, &'static str>) {
        if !self.probes_enabled {
            return;
        }
        let mut probers = self.probers.lock().unwrap();
        let wanted: HashMap<u8, ProbeKey> = spec
            .peers
            .iter()
            .filter(|p| !excluded.contains_key(&p.box_id))
            .map(|p| {
                (
                    p.box_id,
                    ProbeKey { target: spec.probe_addr(p), interval_ms: spec.probe.interval_ms, timeout_ms: spec.probe.timeout_ms },
                )
            })
            .collect();
        probers.retain(|b, (key, h)| {
            let keep = wanted.get(b) == Some(key);
            if !keep {
                h.abort();
            }
            keep
        });
        for (b, key) in wanted {
            if probers.contains_key(&b) {
                continue;
            }
            let client = {
                let mut g = self.icmp.lock().unwrap();
                if g.is_none() {
                    match IcmpClient::new() {
                        Ok(c) => *g = Some(c),
                        Err(e) => {
                            warn!("mesh: cannot open ICMP socket (need CAP_NET_RAW): {e}");
                            return;
                        }
                    }
                }
                g.clone().unwrap()
            };
            let params = ProbeParams { interval: Duration::from_millis(key.interval_ms), timeout: Duration::from_millis(key.timeout_ms) };
            let h = probe::spawn_prober(client, b, key.target, params, self.probe_tx.clone());
            probers.insert(b, (key, h));
        }
    }

    fn stop_all_probers(&self) {
        for (_, (_, h)) in self.probers.lock().unwrap().drain() {
            h.abort();
        }
    }
}

// ---------- helpers ----------

/// Fields the API document leaves out take their value from `[mesh]`.
/// (serde defaults already fill them; this keeps the one network entry
/// consistent with the config's bridge/vni when the body omits `networks`.)
fn merge_defaults(mut spec: MeshSpec, cfg: &MeshSpec) -> MeshSpec {
    if spec.networks.is_empty() {
        spec.networks = cfg.networks.clone();
    }
    spec
}

fn env_for(ops: &dyn NetOps, spec: &MeshSpec) -> validate::Env {
    let local_addrs = ops.local_addrs().unwrap_or_default();
    let underlay_mtu = ops.list_links().ok().and_then(|ls| ls.into_iter().find(|l| l.name == spec.underlay).map(|l| l.mtu));
    validate::Env { local_addrs, underlay_mtu }
}

fn set_condition(conds: &mut Vec<Condition>, kind: ConditionType, status: bool, reason: Option<&str>, message: Option<String>) {
    let now = Utc::now();
    match conds.iter_mut().find(|c| c.kind == kind) {
        Some(c) => {
            if c.status != status {
                c.status = status;
                c.last_transition = now;
            }
            c.reason = reason.map(str::to_owned);
            c.message = message;
        }
        None => conds.push(Condition { kind, status, reason: reason.map(str::to_owned), message, last_transition: now }),
    }
}

/// Atomic write: temp file in the same directory, fsync, rename.
fn persist(path: &Path, spec: &MeshSpec) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec_pretty(spec)?)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn fake() -> Arc<FakeNetOps> {
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        Arc::new(f)
    }
    fn cfg() -> MeshConfig {
        MeshConfig { underlay: "ul0".into(), allow_plain_underlay: true, ..Default::default() }
    }
    fn spec(n: u8) -> MeshSpec {
        MeshSpec {
            enabled: true,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            peers: (1..=n).map(|i| PeerSpec { box_id: i, remote: Ipv4Addr::new(10, 22, i, 1), probe: None }).collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn put_reconcile_then_noop() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        let (s, st) = m.get().await;
        assert!(!s.enabled);
        assert_eq!(st.source, Some(Source::Config));

        let (_, st) = m.put(spec(3), None).await.ok().unwrap();
        assert_eq!(st.generation, 2);
        assert_eq!(st.observed_generation, 2);
        assert_eq!(st.source, Some(Source::Api));
        assert_eq!(st.mtu, 1370);
        assert_eq!(st.peers.len(), 3);
        assert!(st.peers.iter().all(|p| p.fdb_entries == 17 && p.ifindex.is_some()));
        assert!(dir.path().join("mesh.json").exists());
        assert!(f.snapshot().nft_table.is_some());
        let n = f.applied().len();
        assert!(n >= 3 * 6 + 51 + 1, "{n}");

        // Second pass: no kernel writes.
        f.clear_applied();
        m.reconcile_once(false).await;
        assert!(f.applied().is_empty());
        let (_, st) = m.get().await;
        assert_eq!(st.metrics.reconcile_total.get("noop"), Some(&1));
        // Peers are Pending (no probers in tests) so not Ready, not Degraded.
        assert!(!st.is(ConditionType::Ready));
        assert!(!st.is(ConditionType::Degraded));
        assert_eq!(st.condition(ConditionType::Ready).unwrap().reason.as_deref(), Some("PeersNotUp"));
    }

    #[tokio::test]
    async fn idempotent_put_does_not_bump_generation() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        let (_, a) = m.put(spec(2), None).await.ok().unwrap();
        f.clear_applied();
        let (_, b) = m.put(spec(2), None).await.ok().unwrap();
        assert_eq!(a.generation, b.generation);
        assert!(f.applied().is_empty());
    }

    #[tokio::test]
    async fn if_match_conflict_and_validation() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        m.put(spec(1), None).await.ok().unwrap();
        match m.put(spec(2), Some(1)).await {
            Err(PutError::Conflict { current }) => assert_eq!(current, 2),
            _ => panic!("expected conflict"),
        }
        let mut bad = spec(2);
        bad.peers[1].box_id = 0;
        match m.put(bad, None).await {
            Err(PutError::Validation(e)) => assert_eq!(e[0].field, "peers[1].box"),
            _ => panic!("expected validation error"),
        }
        let (s, _) = m.get().await;
        assert_eq!(s.peers.len(), 1, "state unchanged after 400/409");
    }

    #[tokio::test]
    async fn restart_adopts_without_writes() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        {
            let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
            m.put(spec(2), None).await.ok().unwrap();
        }
        let before = f.snapshot();
        f.clear_applied();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        m.reconcile_once(false).await;
        let (s, st) = m.get().await;
        assert!(s.enabled);
        assert_eq!(s.peers.len(), 2);
        assert_eq!(st.source, Some(Source::Api));
        assert!(f.applied().is_empty(), "{:?}", f.applied());
        assert_eq!(f.snapshot().links["gnv-b1"].ifindex, before.links["gnv-b1"].ifindex);
    }

    #[tokio::test]
    async fn delete_tears_down_and_falls_back_to_config() {
        let f = fake();
        f.add_link(kernel::Link {
            ifindex: 0,
            name: "gnv-b9".into(),
            mtu: 1500,
            up: true,
            master: Some("br-int".into()),
            alias: None,
            address: None,
            kind: kernel::LinkKind::Geneve(kernel::GeneveInfo { vni: 23, remote: Some(Ipv4Addr::new(10, 22, 9, 1)), dstport: 6081, ttl: 64, df_set: true }),
        });
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        m.put(spec(2), None).await.ok().unwrap();
        let (st, back) = m.delete().await.unwrap();
        assert!(!back);
        assert_eq!(st.source, Some(Source::Config));
        assert!(st.peers.is_empty());
        let snap = f.snapshot();
        assert!(!snap.links.contains_key("gnv-b1"));
        assert!(!snap.links.contains_key("gnv-b2"));
        assert!(snap.links.contains_key("gnv-b9"), "foreign link survives");
        assert!(snap.nft_table.is_none());
        assert!(!dir.path().join("mesh.json").exists());
        assert_eq!(snap.links["br-int"].mtu, 1370, "teardown does not touch the bridge MTU");
        // Idempotent.
        f.clear_applied();
        m.delete().await.unwrap();
        assert!(f.applied().is_empty());
    }

    #[tokio::test]
    async fn blocking_condition_leaves_kernel_alone() {
        let f = fake();
        f.state.lock().unwrap().geneve_module = false;
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        let (_, st) = m.put(spec(2), None).await.ok().unwrap();
        assert!(st.is(ConditionType::GeneveModuleMissing));
        assert!(!st.is(ConditionType::Ready));
        assert_ne!(st.observed_generation, st.generation);
        assert!(f.applied().is_empty());
        f.state.lock().unwrap().geneve_module = true;
        m.reconcile_once(false).await;
        let (_, st) = m.get().await;
        assert!(!st.is(ConditionType::GeneveModuleMissing));
        assert_eq!(st.observed_generation, st.generation);
    }

    #[tokio::test]
    async fn route_mismatch_marks_error_and_sets_down() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg();
        c.allow_plain_underlay = false;
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&c)).unwrap();
        let mut s = spec(2);
        s.allow_plain_underlay = false;
        f.set_route(Ipv4Addr::new(10, 22, 2, 1), "eth0");
        let (_, st) = m.put(s.clone(), None).await.ok().unwrap();
        let p2 = st.peers.iter().find(|p| p.box_id == 2).unwrap();
        assert_eq!(p2.state, PeerState::Error);
        assert_eq!(p2.reason.as_deref(), Some("UnderlayRouteMismatch"));
        assert!(st.is(ConditionType::Degraded));
        assert!(!f.snapshot().links.contains_key("gnv-b2"), "never created while route is wrong");
        // Route fixed → tunnel created, Error cleared to Pending.
        f.set_route(Ipv4Addr::new(10, 22, 2, 1), "ul0");
        m.reconcile_once(false).await;
        let (_, st) = m.get().await;
        let p2 = st.peers.iter().find(|p| p.box_id == 2).unwrap();
        assert_eq!(p2.state, PeerState::Pending);
        assert!(f.snapshot().links.contains_key("gnv-b2"));
        // Route breaks again → existing tunnel set down, not deleted.
        f.set_route(Ipv4Addr::new(10, 22, 2, 1), "eth0");
        m.reconcile_once(false).await;
        let l = &f.snapshot().links["gnv-b2"];
        assert!(!l.up);
    }

    #[tokio::test]
    async fn config_enabled_comes_back_after_delete() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg();
        c.enabled = true;
        c.peers = vec![PeerSpec { box_id: 5, remote: Ipv4Addr::new(10, 22, 5, 1), probe: None }];
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&c)).unwrap();
        m.reconcile_once(false).await;
        assert!(f.snapshot().links.contains_key("gnv-b5"));
        m.put(spec(1), None).await.ok().unwrap();
        assert!(!f.snapshot().links.contains_key("gnv-b5"));
        assert!(f.snapshot().links.contains_key("gnv-b1"));
        let (st, back) = m.delete().await.unwrap();
        assert!(back);
        assert_eq!(st.source, Some(Source::Config));
        assert!(f.snapshot().links.contains_key("gnv-b5"));
        assert!(!f.snapshot().links.contains_key("gnv-b1"));
    }

    #[test]
    fn invalid_toml_section_fails_startup() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg();
        c.peers = vec![PeerSpec { box_id: 0, remote: Ipv4Addr::new(10, 22, 0, 1), probe: None }];
        let e = MeshManager::new(f, dir.path().into(), Some(&c)).err().unwrap().to_string();
        assert!(e.contains("peers[0].box"), "{e}");
    }

    #[tokio::test]
    async fn probe_events_drive_ready_and_degraded() {
        let f = fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        m.put(spec(2), None).await.ok().unwrap();
        for b in [1u8, 2] {
            for _ in 0..3 {
                m.on_probe(ProbeEvent { box_id: b, ok: true, rtt_ms: Some(1.5) }).await;
            }
        }
        let (_, st) = m.get().await;
        assert!(st.is(ConditionType::Ready), "{:?}", st.conditions);
        assert!(st.peers.iter().all(|p| p.state == PeerState::Up && p.rtt_ms == Some(1.5)));
        for _ in 0..3 {
            m.on_probe(ProbeEvent { box_id: 2, ok: false, rtt_ms: None }).await;
        }
        let (_, st) = m.get().await;
        assert!(st.is(ConditionType::Degraded));
        assert!(!st.is(ConditionType::Ready));
        assert_eq!(st.peers.iter().find(|p| p.box_id == 2).unwrap().state, PeerState::Down);
        assert_eq!(st.metrics.peers["2"].probe_failures_total, 3);
        assert_eq!(st.metrics.peers["2"].up, 0);
    }
}
