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
//!
//! # WireGuard underlay (0.5.0)
//!
//! With `spec.wireguard` set the mesh also owns the underlay: a WireGuard
//! link named `spec.underlay` (same alias rule, plus kind = wireguard),
//! keyed from `<state_dir>/wg.key` (generated once, 0600), addressed
//! `10.22.<box>.1/16`, with one WireGuard peer per mesh peer carrying a
//! `pubkey`. Geneve packets to `10.22.<peer>.1` are therefore always
//! encrypted; `allow_plain_underlay` is moot. Flock's "WireGuard peer up →
//! PUT" ordering collapses into a single PUT carrying `pubkey`/`endpoint`.

pub mod firewall;
pub mod kernel;
pub mod metrics;
pub mod model;
pub mod plan;
pub mod probe;
pub mod validate;
pub mod wg;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify, RwLock, mpsc};
use tracing::{debug, info, warn};

#[cfg(test)]
pub use kernel::FakeNetOps;
pub use kernel::{IpCliNetOps, NetOps};
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
    /// Name of the WireGuard link the last reconcile managed, so a spec
    /// that drops `wireguard` (or a DELETE) can still clean it up.
    last_wg_dev: StdMutex<Option<String>>,
    tap_owners: StdMutex<HashMap<String, String>>,
    pub overlay: Arc<Overlay>,
    /// Set by tests to skip spawning ICMP probers.
    probes_enabled: bool,
}

impl MeshManager {
    /// `config` is the `[mesh]` TOML section (or `None` for all defaults).
    /// If `<state_dir>/mesh.json` exists it wins (source = api).
    pub fn new(
        ops: Arc<dyn NetOps>,
        state_dir: PathBuf,
        config: Option<&MeshConfig>,
    ) -> Result<Arc<Self>> {
        let config_spec = config.cloned().unwrap_or_default().to_spec();
        // TOML validation failures stop the daemon (section 3.2).
        let env = env_for(&*ops, &config_spec);
        validate::validate_or_err(&config_spec, &env)?;

        let path = state_dir.join(STATE_FILE);
        let desired = match std::fs::read(&path) {
            Ok(b) => {
                let spec: MeshSpec = serde_json::from_slice(&b)
                    .with_context(|| format!("parsing {}", path.display()))?;
                let spec = merge_defaults(spec, &config_spec).normalize();
                if let Err(e) = validate::validate_or_err(&spec, &env_for(&*ops, &spec)) {
                    warn!(
                        "{} is invalid ({e:#}); ignoring it and using [mesh] config",
                        path.display()
                    );
                    Desired {
                        spec: config_spec.clone(),
                        source: Source::Config,
                        generation: 1,
                    }
                } else {
                    info!("mesh desired state from {}", path.display());
                    Desired {
                        spec,
                        source: Source::Api,
                        generation: 1,
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Desired {
                spec: config_spec.clone(),
                source: Source::Config,
                generation: 1,
            },
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
            last_wg_dev: StdMutex::new(None),
            tap_owners: StdMutex::new(HashMap::new()),
            overlay: Arc::new(Overlay::default()),
            probes_enabled: true,
        }))
    }

    #[cfg(test)]
    pub fn new_for_test(
        ops: Arc<dyn NetOps>,
        state_dir: PathBuf,
        config: Option<&MeshConfig>,
    ) -> Result<Arc<Self>> {
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
        self.status
            .read()
            .await
            .peers
            .iter()
            .find(|p| p.box_id == box_id)
            .cloned()
    }

    /// Declarative replace. Same body twice is a no-op (no generation bump).
    pub async fn put(
        &self,
        spec: MeshSpec,
        if_match: Option<u64>,
    ) -> Result<(MeshSpec, MeshStatus), PutError> {
        let spec = merge_defaults(spec, &self.config_spec).normalize();
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
                return Err(PutError::Conflict {
                    current: d.generation,
                });
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
            info!(
                generation = d.generation,
                "mesh desired state updated via API"
            );
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
        let mut rx = self
            .probe_rx
            .lock()
            .unwrap()
            .take()
            .expect("run() called twice");
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
            let Some(t) = tracks.get_mut(&ev.box_id) else {
                return;
            };
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
            info!(
                peer = ev.box_id,
                state = state.as_str(),
                "mesh peer state changed"
            );
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
        let observed = match self.ops.observe(&bridge, None) {
            Ok(o) => o,
            Err(e) => {
                warn!("mesh: observing kernel state failed: {e:#}");
                return "error";
            }
        };
        let mut actions = plan::teardown(&observed);
        // The WireGuard link goes last: Geneve tunnels reference it via routes.
        actions.extend(self.wg_teardown_actions(&observed, &d.spec));
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
        st.wireguard = None;
        st.mtu = 0;
        st.metrics.retain_peers(&[]);
        for c in ConditionType::ALL
            .iter()
            .filter(|c| **c != ConditionType::Ready)
        {
            set_condition(&mut st.conditions, *c, false, None, None);
        }
        set_condition(
            &mut st.conditions,
            ConditionType::Ready,
            false,
            Some("Disabled"),
            None,
        );
        if ok {
            st.observed_generation = st.generation;
            if actions.is_empty() { "noop" } else { "ok" }
        } else {
            "error"
        }
    }

    /// Delete every WireGuard link we own among the names the mesh may have
    /// used: the current spec's underlay, the last one reconciled, and the
    /// config's. Ownership (name + alias + kind) is what makes this safe.
    fn wg_teardown_actions(&self, observed: &Observed, spec: &MeshSpec) -> Vec<kernel::Action> {
        let mut names: Vec<String> = vec![];
        if spec.wireguard.is_some() {
            names.push(spec.underlay.clone());
        }
        if let Some(d) = self.last_wg_dev.lock().unwrap().clone() {
            names.push(d);
        }
        if self.config_spec.wireguard.is_some() {
            names.push(self.config_spec.underlay.clone());
        }
        names.sort();
        names.dedup();
        let acts: Vec<_> = names
            .iter()
            .flat_map(|n| wg::teardown(observed, n))
            .collect();
        *self.last_wg_dev.lock().unwrap() = None;
        acts
    }

    /// WireGuard underlay step (runs before the Geneve preflight so the
    /// underlay exists by the time it is looked up). Returns `false` when a
    /// blocking condition was set, `None`-ish errors are reported in `all_ok`.
    fn do_wireguard(&self, spec: &MeshSpec, st: &mut MeshStatus, all_ok: &mut bool) -> bool {
        let Some(w) = &spec.wireguard else {
            set_condition(
                &mut st.conditions,
                ConditionType::WireguardModuleMissing,
                false,
                None,
                None,
            );
            set_condition(
                &mut st.conditions,
                ConditionType::WireguardConflict,
                false,
                None,
                None,
            );
            st.wireguard = None;
            // Spec no longer manages WireGuard: remove a link we built earlier.
            let prev = self.last_wg_dev.lock().unwrap().clone();
            if let Some(prev) = prev
                && let Ok(observed) = self.ops.observe(&spec.bridge(), None)
            {
                for a in wg::teardown(&observed, &prev) {
                    match self.ops.apply(&a) {
                        Ok(()) => {
                            info!(
                                action = a.kind(),
                                dev = a.dev(),
                                "mesh wireguard (no longer managed)"
                            );
                            st.metrics.inc_action(a.kind());
                        }
                        Err(e) => {
                            *all_ok = false;
                            warn!(
                                action = a.kind(),
                                dev = a.dev(),
                                "mesh wireguard teardown failed: {e:#}"
                            );
                        }
                    }
                }
                *self.last_wg_dev.lock().unwrap() = None;
            }
            return true;
        };
        let dev = spec.underlay.as_str();
        *self.last_wg_dev.lock().unwrap() = Some(dev.to_owned());
        let module_ok = self.ops.wireguard_module_present();
        set_condition(&mut st.conditions, ConditionType::WireguardModuleMissing, !module_ok, Some("NoSysModule"),
            (!module_ok).then(|| "/sys/module/wireguard does not exist; the .deb ships /etc/modules-load.d/xdebbie-mesh.conf".to_string()));
        if !module_ok {
            return false;
        }
        let observed = match self.ops.observe(&spec.bridge(), Some(dev)) {
            Ok(o) => o,
            Err(e) => {
                warn!("mesh: observing WireGuard state failed: {e:#}");
                *all_ok = false;
                return true;
            }
        };
        let conflict = match wg::classify(&observed, dev) {
            wg::WgState::Foreign(m) => Some(m),
            _ => None,
        };
        set_condition(
            &mut st.conditions,
            ConditionType::WireguardConflict,
            conflict.is_some(),
            Some("ForeignLink"),
            conflict.clone(),
        );
        if conflict.is_some() {
            return false;
        }
        let (key_path, pubkey) = match ensure_wg_key(&*self.ops, &self.state_dir) {
            Ok(k) => k,
            Err(e) => {
                warn!("mesh: WireGuard key unavailable: {e:#}");
                *all_ok = false;
                return true;
            }
        };
        let actions = wg::diff(wg::WgInput {
            spec,
            observed: &observed,
            key_path: &key_path,
            pubkey: &pubkey,
        });
        for a in &actions {
            match self.ops.apply(a) {
                Ok(()) => {
                    // keys/endpoints are not logged
                    info!(action = a.kind(), dev = a.dev(), "mesh wireguard");
                    st.metrics.inc_action(a.kind());
                }
                Err(e) => {
                    *all_ok = false;
                    warn!(
                        action = a.kind(),
                        dev = a.dev(),
                        "mesh wireguard action failed: {e:#}"
                    );
                }
            }
        }
        // Status from the post-apply view.
        let observed = if actions.is_empty() {
            observed
        } else {
            self.ops
                .observe(&spec.bridge(), Some(dev))
                .unwrap_or(observed)
        };
        let now = self.ops.now_unix();
        let iface = observed.wg.clone().unwrap_or_default();
        let peers = spec
            .peers
            .iter()
            .filter_map(|p| {
                let k = p.pubkey.as_ref()?;
                let c = iface.peer(k);
                Some(WgPeerStatus {
                    box_id: p.box_id,
                    pubkey: k.clone(),
                    endpoint: c
                        .and_then(|c| c.endpoint.clone())
                        .or_else(|| p.endpoint.clone()),
                    handshake_age_s: c
                        .filter(|c| c.latest_handshake > 0)
                        .map(|c| now.saturating_sub(c.latest_handshake)),
                    rx_bytes: c.map(|c| c.rx_bytes).unwrap_or(0),
                    tx_bytes: c.map(|c| c.tx_bytes).unwrap_or(0),
                })
            })
            .collect();
        st.wireguard = Some(WireguardStatus {
            device: dev.to_owned(),
            pubkey,
            listen_port: w.listen_port,
            address: format!(
                "{}/{}",
                spec.wg_address().unwrap_or(Ipv4Addr::UNSPECIFIED),
                w.prefix
            ),
            peers,
        });
        true
    }

    fn do_reconcile(&self, d: &Desired, st: &mut MeshStatus) -> &'static str {
        let spec = &d.spec;
        let bridge = spec.bridge();
        let wg_dev = spec.wireguard.as_ref().map(|_| spec.underlay.as_str());

        // 2. wireguard underlay (0.5.0)
        let mut wg_ok = true;
        if !self.do_wireguard(spec, st, &mut wg_ok) {
            set_condition(
                &mut st.conditions,
                ConditionType::Ready,
                false,
                Some("Blocked"),
                Some("a blocking WireGuard condition is set; kernel left unchanged".into()),
            );
            self.overlay.set(None);
            return "error";
        }

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
        set_condition(
            &mut st.conditions,
            ConditionType::BridgeMissing,
            bridge_link.is_none(),
            Some("NotFound"),
            bridge_link
                .is_none()
                .then(|| format!("{bridge} does not exist or is not a bridge")),
        );
        set_condition(
            &mut st.conditions,
            ConditionType::UnderlayMissing,
            underlay_link.is_none(),
            Some("NotFound"),
            underlay_link
                .is_none()
                .then(|| format!("underlay interface {} does not exist", spec.underlay)),
        );
        let (Some(bridge_link), Some(underlay_link), true) =
            (bridge_link, underlay_link, module_ok)
        else {
            set_condition(
                &mut st.conditions,
                ConditionType::Ready,
                false,
                Some("Blocked"),
                Some("a blocking condition is set; kernel left unchanged".into()),
            );
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
                warn!(
                    "mesh: fixed mtu {m} exceeds computed {computed} (underlay mtu changed?); using {computed}"
                );
                computed
            }
        };
        st.mtu = mtu;
        self.overlay.set(Some((bridge.clone(), mtu)));

        // 5. observe
        let observed = match self.ops.observe(&bridge, wg_dev) {
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
                    debug!(
                        peer = p.box_id,
                        "mesh: route lookup for {} failed: {e:#}", p.remote
                    );
                    if !spec.allow_plain_underlay {
                        route_bad.insert(p.box_id);
                    }
                    route_dev.insert(p.box_id, None);
                }
            }
        }

        // 7. plan
        let plan = plan::diff(plan::PlanInput {
            spec,
            observed: &observed,
            mtu,
            route_bad: &route_bad,
        });

        // 8. apply
        let mut failed: BTreeMap<u8, String> = BTreeMap::new();
        let mut all_ok = wg_ok;
        for a in &plan.actions {
            let peer = parse_tunnel_name(a.dev());
            match self.ops.apply(a) {
                Ok(()) => {
                    info!(action = a.kind(), dev = a.dev(), peer = ?peer, "mesh");
                    st.metrics.inc_action(a.kind());
                }
                Err(e) => {
                    all_ok = false;
                    warn!(
                        action = a.kind(),
                        dev = a.dev(),
                        "mesh action failed: {e:#}"
                    );
                    if let Some(b) = peer {
                        failed
                            .entry(b)
                            .or_insert_with(|| format!("{}: {e}", a.kind()));
                    }
                }
            }
        }

        // 9. firewall
        if spec.firewall {
            let rendered = firewall::render(spec);
            let unchanged = self.last_nft.lock().unwrap().as_deref() == Some(rendered.as_str())
                && self.ops.nft_table_exists();
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

        // 10. bridge checks. STP is forced off by the plan; the condition
        // only stays set if that action failed.
        let stp_was_on = matches!(&bridge_link.kind, kernel::LinkKind::Bridge(b) if b.stp_enabled);
        let stp_fix_failed = stp_was_on
            && plan
                .actions
                .iter()
                .any(|a| matches!(a, kernel::Action::SetBridgeStpOff { .. }))
            && !all_ok;
        set_condition(&mut st.conditions, ConditionType::BridgeStpEnabled, stp_fix_failed, Some("StpOn"),
            stp_fix_failed.then(|| format!("STP is enabled on {bridge} and turning it off failed; the mesh relies on isolated ports, not STP")));
        let want_mac = bridge_mac(spec.box_id);
        let mac_bad = spec.fdb == FdbMode::Derived
            && bridge_link
                .address
                .as_deref()
                .map(|a| !a.eq_ignore_ascii_case(&want_mac))
                .unwrap_or(true);
        set_condition(
            &mut st.conditions,
            ConditionType::BridgeMacMismatch,
            mac_bad,
            Some("DerivedPlan"),
            mac_bad.then(|| {
                format!(
                    "{bridge} has MAC {}, derived plan expects {want_mac}",
                    bridge_link.address.clone().unwrap_or_default()
                )
            }),
        );

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
        let excluded: BTreeMap<u8, &'static str> =
            plan.excluded.iter().map(|e| (e.box_id, e.reason)).collect();
        let name_conflict = plan.excluded.iter().any(|e| e.reason == "NameConflict");
        set_condition(
            &mut st.conditions,
            ConditionType::NameConflict,
            name_conflict,
            Some("ForeignLink"),
            name_conflict.then(|| "a non-mesh interface uses a gnv-b<P> name".to_string()),
        );
        {
            let mut tracks = self.tracks.lock().unwrap();
            let live: Vec<u8> = spec.peers.iter().map(|p| p.box_id).collect();
            tracks.retain(|b, _| live.contains(b));
            for p in &spec.peers {
                let t = tracks
                    .entry(p.box_id)
                    .or_insert_with(|| PeerTrack::new(&spec.probe));
                if t.hyst.up_after != spec.probe.up_after.max(1)
                    || t.hyst.down_after != spec.probe.down_after.max(1)
                {
                    *t = PeerTrack::new(&spec.probe);
                }
                t.route_dev = route_dev.get(&p.box_id).cloned().flatten();
                let err_reason = excluded
                    .get(&p.box_id)
                    .map(|r| r.to_string())
                    .or_else(|| failed.get(&p.box_id).cloned());
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
            self.ops.observe(&bridge, wg_dev).unwrap_or(observed)
        };
        if all_ok {
            st.observed_generation = st.generation;
        }
        self.refresh_peer_rows(st, Some(&observed_after));
        st.metrics
            .retain_peers(&spec.peers.iter().map(|p| p.box_id).collect::<Vec<_>>());
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
            let (state, reason, rtt, last_change, route_dev, failures) = match tracks.get(&p.box_id)
            {
                Some(t) => (
                    t.hyst.state,
                    t.reason.clone(),
                    t.rtt_ms,
                    t.last_change,
                    t.route_dev.clone(),
                    t.failures,
                ),
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
                None => (
                    prev.and_then(|r| r.ifindex),
                    prev.map(|r| r.fdb_entries).unwrap_or(0),
                ),
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
                route_dev: if spec.allow_plain_underlay {
                    route_dev
                } else {
                    None
                },
            });
        }
        st.peers = rows;
        set_condition(
            &mut st.conditions,
            ConditionType::Degraded,
            any_bad,
            Some("PeerDownOrError"),
            any_bad.then(|| "at least one peer is Down or Error".to_string()),
        );
        let blocked = st
            .conditions
            .iter()
            .any(|c| c.status && c.kind.is_blocking());
        let ready = all_up && st.observed_generation == st.generation && !blocked;
        let reason = if blocked {
            "Blocked"
        } else if st.observed_generation != st.generation {
            "Reconciling"
        } else if !all_up {
            "PeersNotUp"
        } else {
            "AllPeersUp"
        };
        set_condition(
            &mut st.conditions,
            ConditionType::Ready,
            ready,
            Some(reason),
            None,
        );
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
                    ProbeKey {
                        target: spec.probe_addr(p),
                        interval_ms: spec.probe.interval_ms,
                        timeout_ms: spec.probe.timeout_ms,
                    },
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
            let params = ProbeParams {
                interval: Duration::from_millis(key.interval_ms),
                timeout: Duration::from_millis(key.timeout_ms),
            };
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
    let underlay_mtu = match &spec.wireguard {
        Some(w) => Some(w.mtu as u32),
        None => ops.list_links().ok().and_then(|ls| {
            ls.into_iter()
                .find(|l| l.name == spec.underlay)
                .map(|l| l.mtu)
        }),
    };
    validate::Env {
        local_addrs,
        underlay_mtu,
    }
}

/// Load or create the WireGuard private key (`<state_dir>/wg.key`, 0600).
/// Returns (path, public key).
fn ensure_wg_key(ops: &dyn NetOps, state_dir: &Path) -> Result<(PathBuf, String)> {
    let path = state_dir.join(WG_KEY_FILE);
    let private = match std::fs::read_to_string(&path) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_owned(),
        Ok(_) | Err(_)
            if !path.exists()
                || std::fs::metadata(&path)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false) =>
        {
            let k = ops.wg_genkey().context("wg genkey")?;
            std::fs::create_dir_all(state_dir)?;
            let mut tmp = tempfile::NamedTempFile::new_in(state_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
            }
            tmp.write_all(format!("{k}\n").as_bytes())?;
            tmp.as_file().sync_all()?;
            tmp.persist(&path)?;
            info!("mesh: generated WireGuard key {}", path.display());
            k
        }
        Ok(k) => k.trim().to_owned(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let public = ops.wg_pubkey(&private).context("wg pubkey")?;
    Ok((path, public))
}

fn set_condition(
    conds: &mut Vec<Condition>,
    kind: ConditionType,
    status: bool,
    reason: Option<&str>,
    message: Option<String>,
) {
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
        None => conds.push(Condition {
            kind,
            status,
            reason: reason.map(str::to_owned),
            message,
            last_transition: now,
        }),
    }
}

/// Atomic write: temp file in the same directory, fsync, rename.
fn persist(path: &Path, spec: &MeshSpec) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
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
    /// An opted-out `[mesh]` (the daemon default is on): what DELETE falls back to.
    fn cfg() -> MeshConfig {
        MeshConfig {
            enabled: false,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            ..Default::default()
        }
    }
    fn off() -> MeshConfig {
        MeshConfig {
            enabled: false,
            ..Default::default()
        }
    }
    fn spec(n: u8) -> MeshSpec {
        MeshSpec {
            enabled: true,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            peers: (1..=n)
                .map(|i| PeerSpec::new(i, Ipv4Addr::new(10, 22, i, 1)))
                .collect(),
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
        assert!(
            st.peers
                .iter()
                .all(|p| p.fdb_entries == 17 && p.ifindex.is_some())
        );
        assert!(dir.path().join("mesh.json").exists());
        assert!(f.snapshot().nft_table.is_some());
        let n = f.applied().len();
        assert!(n > 3 * 6 + 51, "{n}");

        // Second pass: no kernel writes.
        f.clear_applied();
        m.reconcile_once(false).await;
        assert!(f.applied().is_empty());
        let (_, st) = m.get().await;
        assert_eq!(st.metrics.reconcile_total.get("noop"), Some(&1));
        // Peers are Pending (no probers in tests) so not Ready, not Degraded.
        assert!(!st.is(ConditionType::Ready));
        assert!(!st.is(ConditionType::Degraded));
        assert_eq!(
            st.condition(ConditionType::Ready)
                .unwrap()
                .reason
                .as_deref(),
            Some("PeersNotUp")
        );
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
        assert_eq!(
            f.snapshot().links["gnv-b1"].ifindex,
            before.links["gnv-b1"].ifindex
        );
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
            kind: kernel::LinkKind::Geneve(kernel::GeneveInfo {
                vni: 23,
                remote: Some(Ipv4Addr::new(10, 22, 9, 1)),
                dstport: 6081,
                ttl: 64,
                df_set: true,
            }),
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
        assert_eq!(
            snap.links["br-int"].mtu, 1370,
            "teardown does not touch the bridge MTU"
        );
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
        assert!(
            !f.snapshot().links.contains_key("gnv-b2"),
            "never created while route is wrong"
        );
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
        c.peers = vec![PeerSpec::new(5, Ipv4Addr::new(10, 22, 5, 1))];
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
        c.peers = vec![PeerSpec::new(0, Ipv4Addr::new(10, 22, 0, 1))];
        let e = MeshManager::new(f, dir.path().into(), Some(&c))
            .err()
            .unwrap()
            .to_string();
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
                m.on_probe(ProbeEvent {
                    box_id: b,
                    ok: true,
                    rtt_ms: Some(1.5),
                })
                .await;
            }
        }
        let (_, st) = m.get().await;
        assert!(st.is(ConditionType::Ready), "{:?}", st.conditions);
        assert!(
            st.peers
                .iter()
                .all(|p| p.state == PeerState::Up && p.rtt_ms == Some(1.5))
        );
        for _ in 0..3 {
            m.on_probe(ProbeEvent {
                box_id: 2,
                ok: false,
                rtt_ms: None,
            })
            .await;
        }
        let (_, st) = m.get().await;
        assert!(st.is(ConditionType::Degraded));
        assert!(!st.is(ConditionType::Ready));
        assert_eq!(
            st.peers.iter().find(|p| p.box_id == 2).unwrap().state,
            PeerState::Down
        );
        assert_eq!(st.metrics.peers["2"].probe_failures_total, 3);
        assert_eq!(st.metrics.peers["2"].up, 0);
    }

    // ---------- WireGuard (0.5.0) ----------

    const K1: &str = "EtEvrro2LlkElPgm+ge5ndqtzL2o9ehIWwyIFnKsBQU=";
    const K2: &str = "2Mt9XkPn7iVObR5nCjQYhTe1rtVf6hVuvIG1cyL+zGM=";

    fn wg_fake() -> Arc<FakeNetOps> {
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        // a physical uplink; the wg link is created by the mesh
        f.add_underlay("eth0", 1500, Ipv4Addr::new(192, 0, 2, 10));
        Arc::new(f)
    }
    fn wg_spec() -> MeshSpec {
        MeshSpec {
            enabled: true,
            underlay: "wg-flock".into(),
            wireguard: Some(WireguardSpec::default()),
            peers: vec![
                PeerSpec {
                    pubkey: Some(K1.into()),
                    endpoint: Some("192.0.2.11:51820".into()),
                    ..PeerSpec::new(1, Ipv4Addr::UNSPECIFIED)
                },
                PeerSpec {
                    pubkey: Some(K2.into()),
                    ..PeerSpec::new(2, Ipv4Addr::UNSPECIFIED)
                },
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn no_section_means_meshed_with_a_managed_underlay_and_no_peers() {
        // A config without `[mesh]` hands the manager `MeshConfig::default()`
        // (via Config::mesh_config): on, zero peers, WireGuard managed. The
        // box must come up ready to be enrolled: underlay link + key, MTU pinned.
        let f = wg_fake();
        let dir = tempfile::tempdir().unwrap();
        let m =
            MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&MeshConfig::default()))
                .unwrap();
        m.reconcile_once(false).await;
        let (s, st) = m.get().await;
        assert!(s.enabled && s.peers.is_empty() && s.wireguard.is_some());
        assert_eq!(st.source, Some(Source::Config));
        assert_eq!(st.mtu, 1370);
        let w = st.wireguard.as_ref().expect("managed underlay");
        assert_eq!(
            (w.device.as_str(), w.address.as_str()),
            ("wg-flock", "10.22.0.1/16")
        );
        assert!(f.snapshot().links.contains_key("wg-flock"));
        assert!(dir.path().join(WG_KEY_FILE).exists());
        assert!(
            !ConditionType::ALL
                .iter()
                .any(|c| c.is_blocking() && st.is(*c)),
            "{:?}",
            st.conditions
        );
    }

    #[tokio::test]
    async fn wireguard_underlay_is_built_then_restart_is_silent() {
        let f = wg_fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), None).unwrap();
        let (s, st) = m.put(wg_spec(), None).await.ok().unwrap();
        assert_eq!(
            s.peers[0].remote,
            Ipv4Addr::new(10, 22, 1, 1),
            "remote derived"
        );
        assert_eq!(st.observed_generation, st.generation);
        assert_eq!(st.mtu, 1370, "1420 − 50");
        assert!(
            !ConditionType::ALL
                .iter()
                .any(|c| c.is_blocking() && st.is(*c)),
            "{:?}",
            st.conditions
        );
        assert_eq!(
            st.condition(ConditionType::Ready)
                .unwrap()
                .reason
                .as_deref(),
            Some("PeersNotUp")
        );
        assert!(
            st.peers
                .iter()
                .all(|p| p.state == PeerState::Pending && p.ifindex.is_some())
        );
        let w = st.wireguard.as_ref().expect("wireguard status");
        assert_eq!(w.device, "wg-flock");
        assert_eq!(w.pubkey, kernel::fake_pubkey("PRIV1"));
        assert_eq!(w.address, "10.22.0.1/16");
        assert_eq!(w.peers.len(), 2);
        assert_eq!(w.peers[0].handshake_age_s, None);
        // key file persisted 0600
        let kp = dir.path().join(WG_KEY_FILE);
        assert_eq!(std::fs::read_to_string(&kp).unwrap().trim(), "PRIV1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&kp).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // kernel: wg link ours, 2 wg peers with /32 allowed-ips, geneve tunnels present
        let snap = f.snapshot();
        assert!(snap.links["wg-flock"].wg_owned("wg-flock"));
        assert_eq!(snap.wg["wg-flock"].listen_port, 51820);
        assert_eq!(
            snap.wg["wg-flock"].peer(K1).unwrap().allowed_ips,
            vec!["10.22.1.1/32"]
        );
        assert!(snap.links.contains_key("gnv-b1") && snap.links.contains_key("gnv-b2"));
        assert_eq!(snap.keys_generated, 1);

        // handshake shows up in status
        f.wg_handshake("wg-flock", K1, 7);
        m.reconcile_once(false).await;
        let (_, st) = m.get().await;
        let w = st.wireguard.unwrap();
        assert_eq!(w.peers[0].handshake_age_s, Some(7));
        assert_eq!((w.peers[0].rx_bytes, w.peers[0].tx_bytes), (1000, 2000));

        // restart: same key, zero kernel writes
        f.clear_applied();
        drop(m);
        let m2 = MeshManager::new_for_test(f.clone(), dir.path().into(), None).unwrap();
        m2.reconcile_once(false).await;
        assert!(
            f.applied().is_empty(),
            "restart must not touch the kernel: {:?}",
            f.applied()
        );
        assert_eq!(f.snapshot().keys_generated, 1, "key must be reused");
        let (_, st) = m2.get().await;
        assert_eq!(st.wireguard.unwrap().pubkey, kernel::fake_pubkey("PRIV1"));
    }

    #[tokio::test]
    async fn wireguard_peer_removal_and_teardown() {
        let f = wg_fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&off())).unwrap();
        m.put(wg_spec(), None).await.ok().unwrap();
        f.clear_applied();
        let mut s = wg_spec();
        s.peers.truncate(1);
        m.put(s, Some(2)).await.ok().unwrap();
        let kinds: Vec<&str> = f.applied().iter().map(|a| a.kind()).collect();
        assert!(
            kinds.contains(&"wg_peer_del") && kinds.contains(&"delete_tunnel"),
            "{kinds:?}"
        );
        assert!(f.snapshot().wg["wg-flock"].peer(K2).is_none());
        assert!(!f.snapshot().links.contains_key("gnv-b2"));

        let (st, _) = m.delete().await.unwrap();
        let snap = f.snapshot();
        assert!(!snap.links.contains_key("wg-flock"), "wg link torn down");
        assert!(!snap.links.contains_key("gnv-b1"));
        assert!(snap.links.contains_key("eth0") && snap.links.contains_key("br-int"));
        assert!(st.wireguard.is_none());
        assert!(
            dir.path().join(WG_KEY_FILE).exists(),
            "identity survives teardown"
        );
    }

    #[tokio::test]
    async fn foreign_underlay_name_blocks_without_touching_it() {
        let f = wg_fake();
        f.add_underlay("wg-flock", 1420, Ipv4Addr::new(10, 22, 0, 1)); // a veth, not ours
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), None).unwrap();
        f.clear_applied();
        let (_, st) = m.put(wg_spec(), None).await.ok().unwrap();
        assert!(st.is(ConditionType::WireguardConflict));
        assert!(!st.is(ConditionType::Ready));
        assert!(f.applied().is_empty(), "{:?}", f.applied());
        assert!(
            !dir.path().join(WG_KEY_FILE).exists(),
            "no key generated while blocked"
        );

        // module missing is blocking too
        let f2 = wg_fake();
        f2.state.lock().unwrap().wireguard_module = false;
        let dir2 = tempfile::tempdir().unwrap();
        let m2 = MeshManager::new_for_test(f2.clone(), dir2.path().into(), None).unwrap();
        let (_, st) = m2.put(wg_spec(), None).await.ok().unwrap();
        assert!(st.is(ConditionType::WireguardModuleMissing));
        assert!(f2.applied().is_empty());
    }

    #[tokio::test]
    async fn wireguard_peer_without_pubkey_is_rejected() {
        let f = wg_fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), None).unwrap();
        let mut s = wg_spec();
        s.peers[1].pubkey = None;
        match m.put(s, None).await {
            Err(PutError::Validation(e)) => assert_eq!(e[0].field, "peers[1].pubkey"),
            Err(PutError::Conflict { .. }) | Err(PutError::Internal(_)) => panic!("wrong error"),
            Ok(_) => panic!("expected validation error"),
        }
    }

    #[tokio::test]
    async fn dropping_wireguard_from_spec_removes_our_link_only() {
        let f = wg_fake();
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), None).unwrap();
        m.put(wg_spec(), None).await.ok().unwrap();
        assert!(f.snapshot().links.contains_key("wg-flock"));
        // switch to a plain underlay on eth0
        let mut s = wg_spec();
        s.wireguard = None;
        s.underlay = "eth0".into();
        s.allow_plain_underlay = true;
        for p in &mut s.peers {
            p.pubkey = None;
            p.endpoint = None;
            p.remote = Ipv4Addr::new(192, 0, 2, 10 + p.box_id);
        }
        f.clear_applied();
        let (_, st) = m.put(s, None).await.ok().unwrap();
        assert!(
            !f.snapshot().links.contains_key("wg-flock"),
            "old wg link removed"
        );
        assert!(
            f.applied()
                .iter()
                .any(|a| matches!(a, kernel::Action::DeleteLink { dev } if dev == "wg-flock"))
        );
        assert!(st.wireguard.is_none());
        assert!(f.snapshot().links.contains_key("eth0"));
        // and a second pass is silent
        f.clear_applied();
        m.reconcile_once(false).await;
        assert!(f.applied().is_empty(), "{:?}", f.applied());
    }

    #[tokio::test]
    async fn stp_is_forced_off_on_the_mesh_bridge() {
        let f = FakeNetOps::new();
        f.add_bridge_stp("br-int", 1500, "52:54:00:23:00:fe", true);
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        let f = Arc::new(f);
        let dir = tempfile::tempdir().unwrap();
        let m = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg())).unwrap();
        let (_, st) = m.put(spec(1), None).await.ok().unwrap();
        assert!(f.applied().iter().any(
            |a| matches!(a, kernel::Action::SetBridgeStpOff { bridge } if bridge == "br-int")
        ));
        assert!(
            matches!(&f.snapshot().links["br-int"].kind, kernel::LinkKind::Bridge(b) if !b.stp_enabled)
        );
        assert!(
            !st.is(ConditionType::BridgeStpEnabled),
            "{:?}",
            st.condition(ConditionType::BridgeStpEnabled)
        );
        f.clear_applied();
        m.reconcile_once(false).await;
        assert!(f.applied().is_empty());

        // if the kernel refuses, the condition says so
        let f2 = FakeNetOps::new();
        f2.add_bridge_stp("br-int", 1500, "52:54:00:23:00:fe", true);
        f2.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        f2.state.lock().unwrap().fail_on = Some("set_bridge_stp_off");
        let f2 = Arc::new(f2);
        let dir2 = tempfile::tempdir().unwrap();
        let m2 = MeshManager::new_for_test(f2.clone(), dir2.path().into(), Some(&cfg())).unwrap();
        let (_, st) = m2.put(spec(1), None).await.ok().unwrap();
        assert!(st.is(ConditionType::BridgeStpEnabled));
        assert_ne!(st.observed_generation, st.generation);
    }
}
