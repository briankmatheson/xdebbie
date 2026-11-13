// SPDX-License-Identifier: GPL-3.0-or-later
//! The control loop. Each tick: refresh host inventory, place pending VMs
//! one at a time (so each decision sees the previous one), then drive every
//! other VM one step toward its desired state, concurrently.
//!
//! The reconciler never holds the store lock across hypervisor I/O. It
//! computes a `Patch` from a snapshot and merges it, so API writes made in
//! the meantime (notably delete requests) are never lost.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use futures::future::join_all;
use tracing::{info, warn};
use uuid::Uuid;
use xdebbie_core::{
    Address, Attachment, Disk, Host, PowerIntent, Storage, Vm, VmPhase, Volume, place,
};

use crate::{
    config::Config,
    libvirt::{DomState, Hypervisor},
    store::{State, Store},
};

/// ABE bytes per host name reserved by `POST /v1/volumes` calls still in
/// progress: promised, but not yet in the ABE's `provisioned_bytes`.
pub type AbeLedger = Arc<Mutex<BTreeMap<String, u64>>>;

/// A snapshot of the ledger for the scheduler.
pub fn ledger_totals(l: &AbeLedger) -> BTreeMap<String, u64> {
    l.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Hosts as the scheduler should see them: an ABE reading older than
/// `stale_secs` is dropped, so the scheduler (which stays pure) treats the
/// host's ABE capacity as unknown.
pub fn hosts_for_placement(
    hosts: &BTreeMap<String, Host>,
    now: chrono::DateTime<Utc>,
    stale_secs: i64,
) -> Vec<Host> {
    hosts
        .values()
        .map(|h| {
            let mut h = h.clone();
            if h.abe_capacity
                .as_ref()
                .is_some_and(|c| c.is_stale(now, stale_secs))
            {
                h.abe_capacity = None;
            }
            // A host with an `agent` is driven through its own daemon, which
            // runs `nvme connect` on its own box; only agentless remote hosts
            // are out of reach.
            h.abe_remote = h.agent.is_none() && !crate::libvirt::uri_is_local(&h.uri);
            h
        })
        .collect()
}

pub struct Reconciler<H> {
    pub store: Store,
    pub hv: Arc<H>,
    pub cfg: Arc<Config>,
    /// Lets the mesh name VMs (not `vnetN`) in `vms_mtu_mismatch`.
    pub mesh: Option<Arc<crate::mesh::MeshManager>>,
    /// Only the leader schedules and drives hypervisors; followers idle.
    pub leadership: crate::leader::Leadership,
    /// Shared with the API: volume creates in flight, per host.
    pub abe_ledger: AbeLedger,
}

#[derive(Debug, Default)]
pub struct Patch {
    pub remove: bool,
    pub phase: Option<VmPhase>,
    pub host: Option<String>,
    pub addresses: Option<Vec<Address>>,
    /// `Some(None)` clears the message.
    pub message: Option<Option<String>>,
    pub observed_reboot_generation: Option<u64>,
    pub clear_migration: bool,
    /// Network disks allocated/attached so far (persisted even when the
    /// rest of provisioning failed, so nothing is allocated twice).
    pub disks: Option<Vec<Disk>>,
}

impl Patch {
    fn msg(m: impl Into<String>) -> Self {
        Self {
            message: Some(Some(m.into())),
            ..Default::default()
        }
    }
    fn apply(self, vm: &mut Vm) {
        // A delete that raced with this step wins over any phase change.
        if let Some(p) = self.phase
            && (vm.phase != VmPhase::Deleting || p == VmPhase::Deleting)
        {
            vm.phase = p;
        }
        if let Some(h) = self.host {
            vm.host = Some(h);
        }
        if let Some(a) = self.addresses {
            vm.addresses = a;
        }
        if let Some(m) = self.message {
            vm.message = m;
        }
        if let Some(g) = self.observed_reboot_generation {
            vm.observed_reboot_generation = g;
        }
        if self.clear_migration {
            vm.migrate_to = None;
        }
        if let Some(d) = self.disks {
            vm.disks = d;
        }
        vm.updated = Utc::now();
    }
}

#[derive(Debug, Default)]
pub struct VolPatch {
    pub attached: Option<Option<Attachment>>,
    pub clear_attach_to: bool,
    pub message: Option<Option<String>>,
    /// ABE moved the volume: new `(traddr, port)`.
    pub endpoint: Option<(String, String)>,
}

impl VolPatch {
    fn msg(m: impl Into<String>) -> Self {
        Self {
            message: Some(Some(m.into())),
            ..Default::default()
        }
    }
    fn detached() -> Self {
        Self {
            attached: Some(None),
            message: Some(None),
            ..Default::default()
        }
    }
    fn apply(self, v: &mut Volume) {
        let mut changed = false;
        if let Some(a) = self.attached
            && v.attached != a
        {
            v.attached = a;
            changed = true;
        }
        if self.clear_attach_to && v.attach_to.is_some() {
            v.attach_to = None;
            changed = true;
        }
        if let Some(m) = self.message
            && v.message != m
        {
            v.message = m;
            changed = true;
        }
        if let Some((traddr, port)) = self.endpoint
            && (v.traddr != traddr || v.port != port)
        {
            v.traddr = traddr;
            v.port = port;
            changed = true;
        }
        if changed {
            v.updated = Utc::now();
        }
    }
}

impl<H: Hypervisor> Reconciler<H> {
    pub fn healthy(&self, h: &Host) -> bool {
        h.is_healthy(Utc::now(), self.cfg.host_stale_secs)
    }

    pub async fn run(self) {
        let mut t = tokio::time::interval(Duration::from_secs(self.cfg.reconcile_interval_secs));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut leading = None;
        loop {
            t.tick().await;
            let lead = self.leadership.is_leader().await;
            if leading != Some(lead) {
                if lead {
                    info!("leader: reconciler active");
                } else {
                    info!("follower: reconciler idle, the leader decides");
                }
                leading = Some(lead);
            }
            if !lead {
                continue;
            }
            if let Err(e) = self.tick().await {
                warn!(error = format!("{e:#}"), "reconcile tick failed");
            }
        }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        self.inventory().await?;
        self.schedule().await?;
        self.mesh_tap_owners().await;
        let st = self.store.read().await;
        let work = st
            .vms
            .values()
            .filter(|v| v.phase != VmPhase::Pending && v.phase != VmPhase::Failed)
            .map(|vm| async { (vm.id, self.step(&st, vm).await) });
        let patches = join_all(work).await;
        self.store
            .update(|s| {
                for (id, p) in patches {
                    if p.remove {
                        s.vms.remove(&id);
                        info!(%id, "vm deleted");
                        continue;
                    }
                    if let Some(vm) = s.vms.get_mut(&id) {
                        p.apply(vm);
                    }
                }
            })
            .await?;
        self.volumes().await
    }

    /// Drive every standalone volume toward its `attach_to`. Runs after the
    /// VM steps so a VM that just became Running gets its disks this tick.
    async fn volumes(&self) -> anyhow::Result<()> {
        let st = self.store.read().await;
        if st.volumes.is_empty() {
            return Ok(());
        }
        let work = st
            .volumes
            .values()
            .map(|v| async { (v.id.clone(), self.volume_step(&st, v).await) });
        let patches = join_all(work).await;
        self.store
            .update(|s| {
                for (id, p) in patches {
                    if let Some(v) = s.volumes.get_mut(&id) {
                        p.apply(v);
                    }
                }
            })
            .await
    }

    async fn volume_step(&self, st: &State, vol: &Volume) -> VolPatch {
        // The wish, if it still points at a VM that exists and is staying.
        let want = vol
            .attach_to
            .and_then(|id| st.vms.get(&id))
            .filter(|vm| vm.phase != VmPhase::Deleting);
        let wish_stale = vol.attach_to.is_some() && want.is_none();
        if wish_stale {
            warn!(volume = %vol.id, vm = ?vol.attach_to, "attach target is gone; dropping the request");
        }
        // 1. Plugged in where it should not be (wrong VM, VM gone, nothing
        //    wanted): unplug first; attaching happens next tick.
        if let Some(att) = &vol.attached
            && want.map(|vm| vm.id) != Some(att.vm)
        {
            let Some(h) = st.hosts.get(&att.host) else {
                warn!(volume = %vol.id, host = %att.host, "attached on an unregistered host; forgetting the attachment");
                return VolPatch {
                    clear_attach_to: wish_stale,
                    ..VolPatch::detached()
                };
            };
            if !self.healthy(h) {
                return VolPatch::msg(format!("waiting for host {} to detach", h.name));
            }
            return match self.hv.detach_volume(h, vol, att).await {
                Ok(()) => VolPatch {
                    clear_attach_to: wish_stale,
                    ..VolPatch::detached()
                },
                Err(e) => VolPatch::msg(format!("detach: {e:#}")),
            };
        }
        if wish_stale {
            return VolPatch {
                clear_attach_to: true,
                message: Some(None),
                ..Default::default()
            };
        }
        // A volume whose DELETE is in flight (or failed half-way) is never
        // attached; the API already refuses new attach requests for it.
        if vol.deleting {
            return VolPatch::default();
        }
        let Some(vm) = want else {
            return VolPatch::default();
        };
        let Some(h) = vm.host.as_ref().and_then(|h| st.hosts.get(h)) else {
            return VolPatch::msg(format!("waiting for vm {} to be placed", vm.spec.name));
        };
        if !matches!(vm.phase, VmPhase::Running | VmPhase::Stopped) {
            return VolPatch::msg(format!("waiting for vm {} ({:?})", vm.spec.name, vm.phase));
        }
        if !self.healthy(h) {
            return VolPatch::msg(format!("host {} unreachable", h.name));
        }
        // ABE may have re-exported the volume elsewhere since we last saw
        // it; refresh the record before plugging it in.
        let mut disks = vec![vol.disk()];
        let endpoint = match self.hv.refresh_endpoints(h, &mut disks).await {
            Ok(true) => disks.first().map(|d| (d.traddr.clone(), d.port.clone())),
            Ok(false) => None,
            Err(e) => return VolPatch::msg(format!("refresh endpoint: {e:#}")),
        };
        let mut fresh = vol.clone();
        if let Some((traddr, port)) = &endpoint {
            fresh.traddr = traddr.clone();
            fresh.port = port.clone();
        }
        match self.hv.attach_volume(h, vm, &fresh).await {
            Ok(target) => VolPatch {
                attached: Some(Some(Attachment {
                    vm: vm.id,
                    host: h.name.clone(),
                    target,
                })),
                message: Some(None),
                endpoint,
                ..Default::default()
            },
            Err(e) => VolPatch {
                endpoint,
                ..VolPatch::msg(format!("attach: {e:#}"))
            },
        }
    }

    /// If the mesh sees bridge ports with a stale MTU, tell it which VM
    /// each tap belongs to so `vms_mtu_mismatch` names VMs. Only queries
    /// libvirt while there is something to resolve.
    async fn mesh_tap_owners(&self) {
        let Some(mesh) = &self.mesh else { return };
        let ports = mesh.mismatched_ports().await;
        if ports.is_empty() {
            return;
        }
        // Already resolved entries look like "vm (vnetN)".
        if ports.iter().all(|p| p.contains('(')) {
            return;
        }
        let hosts: Vec<Host> = self.store.read().await.hosts.into_values().collect();
        let mut map = std::collections::HashMap::new();
        for h in &hosts {
            match self.hv.tap_owners(h).await {
                Ok(m) => map.extend(m),
                Err(e) => warn!(host = %h.name, "tap owners: {e:#}"),
            }
        }
        mesh.set_tap_owners(map);
        mesh.trigger();
    }

    async fn inventory(&self) -> anyhow::Result<()> {
        let hosts: Vec<Host> = self.store.read().await.hosts.into_values().collect();
        let res = join_all(hosts.iter().map(|h| async {
            (
                h.name.clone(),
                self.hv.inventory(h).await,
                self.hv.abe_utilization(h).await,
            )
        }))
        .await;
        self.store
            .update(|s| {
                for (name, r, a) in res {
                    let Some(h) = s.hosts.get_mut(&name) else {
                        continue;
                    };
                    match r {
                        Ok(c) => {
                            h.capacity = Some(c);
                            h.last_seen = Some(Utc::now());
                        }
                        Err(e) => warn!(host = %name, error = format!("{e:#}"), "inventory failed"),
                    }
                    match a {
                        // `seen` is the ABE reading's own time; an error
                        // keeps the old reading until it goes stale.
                        Ok(Some(c)) => h.abe_capacity = Some(c),
                        Ok(None) => {}
                        Err(e) => warn!(host = %name, error = format!("{e:#}"), "abe utilization failed; keeping the last reading"),
                    }
                }
            })
            .await
    }

    async fn schedule(&self) -> anyhow::Result<()> {
        let pending: Vec<Uuid> = self
            .store
            .read()
            .await
            .vms
            .values()
            .filter(|v| v.phase == VmPhase::Pending)
            .map(|v| v.id)
            .collect();
        for id in pending {
            let policy = self.cfg.policy;
            let stale = self.cfg.host_stale_secs;
            let inflight = ledger_totals(&self.abe_ledger);
            self.store
                .update(|s| {
                    let hosts = hosts_for_placement(&s.hosts, Utc::now(), stale);
                    let vms: Vec<Vm> = s.vms.values().cloned().collect();
                    let Some(vm) = s.vms.get_mut(&id) else { return };
                    if vm.phase != VmPhase::Pending {
                        return;
                    }
                    match place(
                        &vm.spec,
                        &hosts,
                        &vms,
                        policy,
                        |h| self.healthy(h),
                        Some(id),
                        &inflight,
                    ) {
                        Ok(r) => {
                            info!(vm = %vm.spec.name, host = %r[0].host, "placed");
                            vm.host = Some(r[0].host.clone());
                            vm.phase = VmPhase::Provisioning;
                            vm.message = None;
                        }
                        Err(e) => vm.message = Some(e.to_string()),
                    }
                    vm.updated = Utc::now();
                })
                .await?;
        }
        Ok(())
    }

    async fn step(&self, st: &State, vm: &Vm) -> Patch {
        let host = vm.host.as_ref().and_then(|h| st.hosts.get(h));
        if vm.phase == VmPhase::Deleting {
            let Some(h) = host else {
                if !vm.disks.is_empty() {
                    tracing::warn!(vm = %vm.spec.name, "deleting without a host; its abe volumes are left behind: {:?}",
                        vm.disks.iter().map(|d| format!("{} {}", d.server, d.id)).collect::<Vec<_>>());
                }
                return Patch {
                    remove: true,
                    ..Default::default()
                };
            };
            if !self.healthy(h) {
                return Patch::msg(format!("waiting for host {} to delete", h.name));
            }
            return match self.hv.destroy(h, vm).await {
                Ok(()) => Patch {
                    remove: true,
                    ..Default::default()
                },
                Err(e) => Patch::msg(e.to_string()),
            };
        }
        let Some(h) = host else {
            return Patch {
                phase: Some(VmPhase::Failed),
                message: Some(Some("assigned host no longer registered".into())),
                ..Default::default()
            };
        };
        if !self.healthy(h) {
            return Patch::msg(format!("host {} unreachable", h.name));
        }

        match vm.phase {
            VmPhase::Provisioning => {
                let mut vm = vm.clone();
                let mut p = Patch::default();
                if vm.spec.storage == Storage::Abe {
                    let mut disks = vm.disks.clone();
                    let r = self.hv.refresh_endpoints(h, &mut disks).await;
                    if disks != vm.disks {
                        p.disks = Some(disks.clone());
                        vm.disks = disks.clone();
                    }
                    if let Err(e) = r {
                        p.message = Some(Some(format!("refresh endpoints: {e:#}")));
                        return p;
                    }
                    let r = self.hv.prepare_disks(h, &vm, &mut disks).await;
                    if disks != vm.disks {
                        p.disks = Some(disks.clone());
                        vm.disks = disks;
                    }
                    if let Err(e) = r {
                        p.message = Some(Some(format!("{e:#}")));
                        return p;
                    }
                }
                if let Err(e) = self.hv.provision(h, &vm).await {
                    p.message = Some(Some(e.to_string()));
                    return p;
                }
                if vm.power == PowerIntent::On
                    && let Err(e) = self.hv.start(h, &vm).await
                {
                    p.message = Some(Some(e.to_string()));
                    return p;
                }
                info!(vm = %vm.spec.name, host = %h.name, "provisioned");
                p.phase = Some(if vm.power == PowerIntent::On {
                    VmPhase::Running
                } else {
                    VmPhase::Stopped
                });
                p.message = Some(None);
                p.observed_reboot_generation = Some(vm.reboot_generation);
                p
            }
            VmPhase::Migrating => {
                let Some(dst) = vm.migrate_to.as_ref().and_then(|d| st.hosts.get(d)) else {
                    return Patch {
                        phase: Some(VmPhase::Running),
                        clear_migration: true,
                        message: Some(Some("migration target vanished".into())),
                        ..Default::default()
                    };
                };
                match self.hv.migrate(h, dst, vm).await {
                    Ok(()) => {
                        info!(vm = %vm.spec.name, from = %h.name, to = %dst.name, "migrated");
                        Patch {
                            phase: Some(VmPhase::Running),
                            host: Some(dst.name.clone()),
                            clear_migration: true,
                            message: Some(None),
                            ..Default::default()
                        }
                    }
                    Err(e) => Patch {
                        phase: Some(VmPhase::Running),
                        clear_migration: true,
                        message: Some(Some(format!("migration failed: {e}"))),
                        ..Default::default()
                    },
                }
            }
            VmPhase::Running | VmPhase::Stopped => self.converge(h, vm).await,
            VmPhase::Pending | VmPhase::Failed | VmPhase::Deleting => Patch::default(),
        }
    }

    async fn converge(&self, h: &Host, vm: &Vm) -> Patch {
        let state = match self.hv.state(h, vm).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Patch {
                    phase: Some(VmPhase::Provisioning),
                    message: Some(Some("domain missing; recreating".into())),
                    ..Default::default()
                };
            }
            Err(e) => return Patch::msg(e.to_string()),
        };
        let mut p = Patch::default();
        let mut fresh;
        let vm = if vm.power == PowerIntent::On
            && state == DomState::Off
            && vm.spec.storage == Storage::Abe
        {
            // Starting again after a host or exporter restart: the disks
            // may have moved port.
            fresh = vm.clone();
            match self.hv.refresh_endpoints(h, &mut fresh.disks).await {
                Ok(true) => p.disks = Some(fresh.disks.clone()),
                Ok(false) => {}
                Err(e) => {
                    return Patch::msg(format!("refresh endpoints: {e:#}"));
                }
            }
            &fresh
        } else {
            vm
        };
        let r = match (vm.power, state) {
            (PowerIntent::On, DomState::Running)
                if vm.reboot_generation != vm.observed_reboot_generation =>
            {
                p.observed_reboot_generation = Some(vm.reboot_generation);
                self.hv.reboot(h, vm).await
            }
            (PowerIntent::On, DomState::Off) => self.hv.start(h, vm).await,
            (PowerIntent::Off, DomState::Running) => self.hv.shutdown(h, vm, false).await,
            _ => Ok(()),
        };
        if let Err(e) = r {
            p.message = Some(Some(e.to_string()));
            return p;
        }
        let running = state == DomState::Running;
        p.phase = Some(if running {
            VmPhase::Running
        } else {
            VmPhase::Stopped
        });
        if running && let Ok(mut a) = self.hv.addresses(h, vm).await {
            // The allocated overlay address is authoritative and always
            // listed first, whatever the guest agent / ARP table saw.
            if let Some(ip) = vm.ip {
                a.retain(|x| x.address != ip.to_string());
                a.insert(
                    0,
                    Address {
                        kind: "InternalIP".into(),
                        address: ip.to_string(),
                    },
                );
            }
            a.push(Address {
                kind: "Hostname".into(),
                address: vm.spec.name.clone(),
            });
            p.addresses = Some(a);
        }
        p.message = Some(None);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libvirt::DomState;
    use std::{collections::BTreeMap, sync::Mutex};
    use xdebbie_core::{Capacity, Placement, VmSpec};

    fn test_leader() -> crate::leader::Leadership {
        crate::leader::Leadership::standalone("test", "http://127.0.0.1:7443")
    }

    #[derive(Default)]
    struct Fake {
        doms: Mutex<BTreeMap<(String, String), DomState>>,
        /// abe simulation: how many volumes were allocated / released, and
        /// whether the next populate should fail (once).
        allocated: Mutex<u32>,
        released: Mutex<Vec<String>>,
        fail_populate_once: Mutex<bool>,
        /// (domain, volume id) pairs currently plugged in.
        plugged: Mutex<Vec<(String, String)>>,
        /// abe "moved" these ids to a new port: id -> port.
        moved: Mutex<BTreeMap<String, String>>,
        /// endpoint refreshes that found a change: ids.
        refreshed: Mutex<Vec<String>>,
        /// what `abe_utilization` answers for hosts with an abe.
        abe_cap: Mutex<Option<xdebbie_core::AbeCapacity>>,
    }
    impl Hypervisor for Fake {
        async fn abe_utilization(
            &self,
            h: &Host,
        ) -> anyhow::Result<Option<xdebbie_core::AbeCapacity>> {
            if h.abe.is_none() {
                return Ok(None);
            }
            Ok(self.abe_cap.lock().unwrap().clone())
        }
        async fn refresh_endpoints(&self, _: &Host, disks: &mut Vec<Disk>) -> anyhow::Result<bool> {
            let moved = self.moved.lock().unwrap();
            let mut changed = false;
            for d in disks.iter_mut() {
                if let Some(p) = moved.get(&d.id)
                    && *p != d.port
                {
                    d.port = p.clone();
                    self.refreshed.lock().unwrap().push(d.id.clone());
                    changed = true;
                }
            }
            Ok(changed)
        }
        async fn attach_volume(&self, _: &Host, v: &Vm, vol: &Volume) -> anyhow::Result<String> {
            let mut p = self.plugged.lock().unwrap();
            let key = (v.domain_name(), vol.id.clone());
            if !p.contains(&key) {
                p.push(key);
            }
            Ok("vdc".into())
        }
        async fn detach_volume(
            &self,
            _: &Host,
            vol: &Volume,
            _: &Attachment,
        ) -> anyhow::Result<()> {
            self.plugged.lock().unwrap().retain(|(_, id)| *id != vol.id);
            Ok(())
        }
        async fn prepare_disks(
            &self,
            h: &Host,
            v: &Vm,
            disks: &mut Vec<Disk>,
        ) -> anyhow::Result<()> {
            if v.spec.storage != Storage::Abe {
                return Ok(());
            }
            let server = h
                .abe
                .clone()
                .ok_or_else(|| anyhow::anyhow!("host {} has no ABE", h.name))?;
            if !disks.iter().any(|d| d.role == xdebbie_core::DiskRole::Root) {
                let mut n = self.allocated.lock().unwrap();
                *n += 1;
                disks.push(Disk {
                    role: xdebbie_core::DiskRole::Root,
                    server,
                    traddr: "10.22.0.1".into(),
                    port: "44001".into(),
                    id: format!("{:032x}", *n),
                    bytes: v.spec.disk_gib << 30,
                    populated: false,
                    origin: None,
                });
            }
            let mut fail = self.fail_populate_once.lock().unwrap();
            if *fail {
                *fail = false;
                anyhow::bail!("qemu-img convert: boom");
            }
            for d in disks.iter_mut() {
                d.populated = true;
            }
            Ok(())
        }
        async fn inventory(&self, _: &Host) -> anyhow::Result<Capacity> {
            Ok(Capacity {
                cpus: 16,
                memory_mib: 65536,
                disk_free_gib: 500,
            })
        }
        async fn state(&self, h: &Host, v: &Vm) -> anyhow::Result<Option<DomState>> {
            Ok(self
                .doms
                .lock()
                .unwrap()
                .get(&(h.name.clone(), v.domain_name()))
                .copied())
        }
        async fn provision(&self, h: &Host, v: &Vm) -> anyhow::Result<()> {
            self.doms
                .lock()
                .unwrap()
                .entry((h.name.clone(), v.domain_name()))
                .or_insert(DomState::Off);
            Ok(())
        }
        async fn start(&self, h: &Host, v: &Vm) -> anyhow::Result<()> {
            self.doms
                .lock()
                .unwrap()
                .insert((h.name.clone(), v.domain_name()), DomState::Running);
            Ok(())
        }
        async fn shutdown(&self, h: &Host, v: &Vm, _: bool) -> anyhow::Result<()> {
            self.doms
                .lock()
                .unwrap()
                .insert((h.name.clone(), v.domain_name()), DomState::Off);
            Ok(())
        }
        async fn reboot(&self, _: &Host, _: &Vm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn destroy(&self, h: &Host, v: &Vm) -> anyhow::Result<()> {
            self.doms
                .lock()
                .unwrap()
                .remove(&(h.name.clone(), v.domain_name()));
            self.released
                .lock()
                .unwrap()
                .extend(v.disks.iter().map(|d| d.id.clone()));
            Ok(())
        }
        async fn tap_owners(
            &self,
            _: &Host,
        ) -> anyhow::Result<std::collections::HashMap<String, String>> {
            Ok(Default::default())
        }
        async fn addresses(&self, _: &Host, _: &Vm) -> anyhow::Result<Vec<Address>> {
            Ok(vec![Address {
                kind: "InternalIP".into(),
                address: "10.0.0.99".into(),
            }])
        }
        async fn migrate(&self, s: &Host, d: &Host, v: &Vm) -> anyhow::Result<()> {
            let mut m = self.doms.lock().unwrap();
            let st = m.remove(&(s.name.clone(), v.domain_name())).unwrap();
            m.insert((d.name.clone(), v.domain_name()), st);
            Ok(())
        }
    }

    fn cfg() -> Config {
        toml::from_str("state_file = 'unused'").unwrap()
    }
    fn abe_cap(bytes_gib: u64, provisioned_gib: u64) -> xdebbie_core::AbeCapacity {
        xdebbie_core::AbeCapacity {
            thin: true,
            bytes: bytes_gib << 30,
            used_bytes: 0,
            free_bytes: bytes_gib << 30,
            provisioned_bytes: provisioned_gib << 30,
            data_percent: 1.0,
            metadata_percent: Some(1.0),
            tls: false,

            seen: Utc::now(),
        }
    }
    fn abe_spec(name: &str, disk_gib: u64) -> VmSpec {
        VmSpec {
            name: name.into(),
            vcpus: 1,
            memory_mib: 512,
            disk_gib,
            data_disk_gib: None,
            ip: None,
            storage: Storage::Abe,
            image: "img".into(),
            placement: Default::default(),
            user_data: None,
            meta_data: Default::default(),
            labels: Default::default(),
        }
    }
    fn host(n: &str, fd: &str) -> Host {
        serde_json::from_value(
            serde_json::json!({"name": n, "uri": "test:///", "failureDomain": fd}),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn lifecycle_create_spread_migrate_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        store
            .update(|s| {
                for (n, fd) in [("a", "r1"), ("b", "r2")] {
                    s.hosts.insert(n.into(), host(n, fd));
                }
            })
            .await
            .unwrap();
        let r = Reconciler {
            store: store.clone(),
            hv: Arc::new(Fake::default()),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let mut ids = vec![];
        for i in 0..2 {
            let id = Uuid::new_v4();
            ids.push(id);
            let spec = VmSpec {
                name: format!("cp{i}"),
                vcpus: 2,
                memory_mib: 4096,
                disk_gib: 20,
                data_disk_gib: None,
                ip: None,
                storage: Default::default(),
                image: "img".into(),
                placement: Placement {
                    anti_affinity_group: Some("cp".into()),
                    ..Default::default()
                },
                user_data: None,
                meta_data: Default::default(),
                labels: Default::default(),
            };
            let now = Utc::now();
            store
                .update(|s| {
                    s.vms.insert(
                        id,
                        Vm {
                            id,
                            spec,
                            phase: VmPhase::Pending,
                            power: PowerIntent::On,
                            host: None,
                            addresses: vec![],
                            ip: None,
                            disks: Vec::new(),
                            message: None,
                            migrate_to: None,
                            reboot_generation: 0,
                            observed_reboot_generation: 0,
                            created: now,
                            updated: now,
                        },
                    );
                })
                .await
                .unwrap();
        }
        r.tick().await.unwrap(); // inventory + place + provision/start
        r.tick().await.unwrap(); // converge: addresses
        let s = store.read().await;
        let hosts: Vec<_> = ids.iter().map(|i| s.vms[i].host.clone().unwrap()).collect();
        assert_ne!(hosts[0], hosts[1], "anti-affinity spreads across hosts");
        assert!(ids.iter().all(|i| s.vms[i].phase == VmPhase::Running));
        assert_eq!(s.vms[&ids[0]].addresses[0].address, "10.0.0.99");

        // migrate cp0 to the other host (strict AA off, so it is allowed)
        let other = hosts[1].clone();
        store
            .update(|s| {
                let v = s.vms.get_mut(&ids[0]).unwrap();
                v.migrate_to = Some(other.clone());
                v.phase = VmPhase::Migrating;
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        assert_eq!(
            store.read().await.vms[&ids[0]].host.as_deref(),
            Some(other.as_str())
        );

        // delete
        store
            .update(|s| {
                for id in &ids {
                    s.vms.get_mut(id).unwrap().phase = VmPhase::Deleting;
                }
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        assert!(store.read().await.vms.is_empty());
        assert!(r.hv.doms.lock().unwrap().is_empty());
    }

    fn vm(spec: VmSpec) -> Vm {
        let now = Utc::now();
        Vm {
            id: Uuid::new_v4(),
            spec,
            phase: VmPhase::Pending,
            power: PowerIntent::On,
            host: None,
            addresses: vec![],
            ip: None,
            disks: Vec::new(),
            message: None,
            migrate_to: None,
            reboot_generation: 0,
            observed_reboot_generation: 0,
            created: now,
            updated: now,
        }
    }

    #[tokio::test]
    async fn abe_disks_survive_a_failed_provision_and_are_released_on_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        let mut h = host("a", "r1");
        h.abe = Some("http://10.22.0.1:80".into());
        store
            .update(|s| {
                s.hosts.insert("a".into(), h);
            })
            .await
            .unwrap();
        let fake = Arc::new(Fake::default());
        *fake.abe_cap.lock().unwrap() = Some(abe_cap(100, 0));
        *fake.fail_populate_once.lock().unwrap() = true;
        let r = Reconciler {
            store: store.clone(),
            hv: fake.clone(),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let v = vm(VmSpec {
            name: "abe-vm".into(),
            vcpus: 1,
            memory_mib: 512,
            disk_gib: 8,
            data_disk_gib: None,
            ip: None,
            storage: Storage::Abe,
            image: "img".into(),
            placement: Default::default(),
            user_data: None,
            meta_data: Default::default(),
            labels: Default::default(),
        });
        let id = v.id;
        store
            .update(|s| {
                s.vms.insert(id, v);
            })
            .await
            .unwrap();

        r.tick().await.unwrap(); // place + prepare_disks fails after allocating
        {
            let s = store.read().await;
            let v = &s.vms[&id];
            assert_eq!(v.phase, VmPhase::Provisioning);
            assert_eq!(v.disks.len(), 1, "allocation persisted despite the failure");
            assert!(!v.disks[0].populated);
            assert!(v.message.as_deref().unwrap().contains("boom"));
        }
        r.tick().await.unwrap(); // retry: no new allocation, populate, provision, start
        {
            let s = store.read().await;
            let v = &s.vms[&id];
            assert_eq!(v.phase, VmPhase::Running);
            assert_eq!(*fake.allocated.lock().unwrap(), 1, "no second volume");
            assert!(v.disks[0].populated);
            assert_eq!(v.message, None);
        }
        store
            .update(|s| {
                s.vms.get_mut(&id).unwrap().phase = VmPhase::Deleting;
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        assert!(store.read().await.vms.is_empty());
        assert_eq!(
            fake.released.lock().unwrap().as_slice(),
            [format!("{:032x}", 1)]
        );
    }

    #[tokio::test]
    async fn abe_vm_on_a_host_without_abe_stays_pending_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        store
            .update(|s| {
                s.hosts.insert("a".into(), host("a", "r1"));
            })
            .await
            .unwrap();
        let r = Reconciler {
            store: store.clone(),
            hv: Arc::new(Fake::default()),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let v = vm(abe_spec("abe-vm", 8));
        let id = v.id;
        store
            .update(|s| {
                s.vms.insert(id, v);
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        let s = store.read().await;
        assert_eq!(
            s.vms[&id].phase,
            VmPhase::Pending,
            "never placed, never stuck in Provisioning"
        );
        assert!(
            s.vms[&id].message.as_deref().unwrap().contains("no abe"),
            "{:?}",
            s.vms[&id].message
        );
        assert!(s.vms[&id].host.is_none());
    }

    #[tokio::test]
    async fn abe_capacity_comes_from_inventory_and_gates_placement() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        let mut h = host("a", "r1");
        h.abe = Some("http://10.22.1.1:80".into());
        store
            .update(|s| {
                s.hosts.insert("a".into(), h);
            })
            .await
            .unwrap();
        let fake = Arc::new(Fake::default());
        let r = Reconciler {
            store: store.clone(),
            hv: fake.clone(),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let v = vm(abe_spec("big", 10));
        let id = v.id;
        store
            .update(|s| {
                s.vms.insert(id, v);
            })
            .await
            .unwrap();
        // 1. the abe answers nothing useful yet: capacity unknown
        r.tick().await.unwrap();
        {
            let s = store.read().await;
            assert_eq!(s.hosts["a"].abe_capacity, None);
            assert_eq!(s.vms[&id].phase, VmPhase::Pending);
            assert!(
                s.vms[&id]
                    .message
                    .as_deref()
                    .unwrap()
                    .contains("abe capacity unknown")
            );
        }
        // 2. a 15 GiB pool with 10 GiB promised: 10 more do not fit
        *fake.abe_cap.lock().unwrap() = Some(abe_cap(15, 10));
        r.tick().await.unwrap();
        {
            let s = store.read().await;
            assert_eq!(s.hosts["a"].abe_capacity.as_ref().unwrap().bytes, 15 << 30);
            assert_eq!(s.vms[&id].phase, VmPhase::Pending);
            assert_eq!(
                s.vms[&id].message.as_deref(),
                Some("no host satisfies the request: 1×abe capacity")
            );
        }
        // 3. the ledger (a volume create in flight) counts too
        *fake.abe_cap.lock().unwrap() = Some(abe_cap(15, 0));
        r.abe_ledger.lock().unwrap().insert("a".into(), 6 << 30);
        r.tick().await.unwrap();
        assert_eq!(store.read().await.vms[&id].phase, VmPhase::Pending);
        r.abe_ledger.lock().unwrap().clear();
        // 4. room: placed and provisioned
        r.tick().await.unwrap();
        assert_eq!(store.read().await.vms[&id].phase, VmPhase::Running);
        // 5. a stale reading counts as unknown for the next VM
        store
            .update(|s| {
                s.hosts
                    .get_mut("a")
                    .unwrap()
                    .abe_capacity
                    .as_mut()
                    .unwrap()
                    .seen = Utc::now() - chrono::Duration::seconds(3600);
            })
            .await
            .unwrap();
        *fake.abe_cap.lock().unwrap() = None; // inventory keeps the old (stale) reading
        let v2 = vm(abe_spec("second", 1));
        let id2 = v2.id;
        store
            .update(|s| {
                s.vms.insert(id2, v2);
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        let s = store.read().await;
        assert_eq!(s.vms[&id2].phase, VmPhase::Pending);
        assert!(
            s.vms[&id2]
                .message
                .as_deref()
                .unwrap()
                .contains("abe capacity unknown")
        );
    }

    #[tokio::test]
    async fn moved_exports_are_refreshed_before_a_start_and_before_an_attach() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        let mut h = host("a", "r1");
        h.abe = Some("http://10.22.1.1:80".into());
        store
            .update(|s| {
                s.hosts.insert("a".into(), h);
            })
            .await
            .unwrap();
        let fake = Arc::new(Fake::default());
        *fake.abe_cap.lock().unwrap() = Some(abe_cap(100, 0));
        let r = Reconciler {
            store: store.clone(),
            hv: fake.clone(),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let v = vm(abe_spec("mv", 8));
        let id = v.id;
        let now = Utc::now();
        let vol = Volume {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "pv".into(),
            server: "http://10.22.1.1:80".into(),
            traddr: "10.22.1.1".into(),
            port: "44002".into(),
            bytes: 1,
            perf: None,
            attach_to: Some(id),
            attached: None,
            message: None,
            deleting: false,
            created: now,
            updated: now,
        };
        let vid = vol.id.clone();
        store
            .update(|s| {
                s.vms.insert(id, v);
                s.volumes.insert(vid.clone(), vol);
            })
            .await
            .unwrap();
        r.tick().await.unwrap(); // placed, disks made on 44001, running; volume attached
        let rid = store.read().await.vms[&id].disks[0].id.clone();
        assert_eq!(
            store.read().await.volumes[&vid]
                .attached
                .as_ref()
                .map(|a| a.vm),
            Some(id)
        );

        // abe restarted and re-exported both elsewhere; the domain is off
        fake.moved
            .lock()
            .unwrap()
            .insert(rid.clone(), "44017".into());
        fake.moved
            .lock()
            .unwrap()
            .insert(vid.clone(), "44018".into());
        fake.doms
            .lock()
            .unwrap()
            .insert(("a".into(), "mv".into()), DomState::Off);
        // ... and the hot-plug did not survive the restart either.
        store
            .update(|s| {
                if let Some(v) = s.volumes.get_mut(&vid) {
                    v.attached = None;
                }
            })
            .await
            .unwrap();
        r.tick().await.unwrap(); // observes Off, refreshes, starts
        r.tick().await.unwrap(); // observes Running
        let s = store.read().await;
        assert_eq!(s.vms[&id].phase, VmPhase::Running);
        assert_eq!(
            s.vms[&id].disks[0].port, "44017",
            "record follows the export"
        );
        assert_eq!(
            (
                s.volumes[&vid].port.as_str(),
                s.volumes[&vid].traddr.as_str()
            ),
            ("44018", "10.22.1.1")
        );
        assert_eq!(fake.refreshed.lock().unwrap().as_slice(), [rid, vid]);
    }

    #[tokio::test]
    async fn a_deleting_volume_is_never_attached() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        store
            .update(|s| {
                s.hosts.insert("a".into(), host("a", "r1"));
            })
            .await
            .unwrap();
        let fake = Arc::new(Fake::default());
        let r = Reconciler {
            store: store.clone(),
            hv: fake.clone(),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let v = vm(VmSpec {
            storage: Storage::Local,
            ..abe_spec("one", 8)
        });
        let id = v.id;
        let now = Utc::now();
        let vol = Volume {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "pv".into(),
            server: "http://abe".into(),
            traddr: "10.22.0.1".into(),
            port: "44002".into(),
            bytes: 1,
            perf: None,
            attach_to: Some(id),
            attached: None,
            message: None,
            deleting: true,
            created: now,
            updated: now,
        };
        let vid = vol.id.clone();
        store
            .update(|s| {
                s.vms.insert(id, v);
                s.volumes.insert(vid.clone(), vol);
            })
            .await
            .unwrap();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        let s = store.read().await;
        assert_eq!(s.vms[&id].phase, VmPhase::Running);
        assert_eq!(s.volumes[&vid].attached, None);
        assert!(fake.plugged.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn volume_follows_attach_to_and_lets_go_when_the_vm_dies() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("s.json")).unwrap();
        store
            .update(|s| {
                s.hosts.insert("a".into(), host("a", "r1"));
            })
            .await
            .unwrap();
        let fake = Arc::new(Fake::default());
        let r = Reconciler {
            store: store.clone(),
            hv: fake.clone(),
            cfg: Arc::new(cfg()),
            mesh: None,
            leadership: test_leader(),
            abe_ledger: Arc::default(),
        };
        let mk = |name: &str| {
            vm(VmSpec {
                name: name.into(),
                vcpus: 1,
                memory_mib: 512,
                disk_gib: 8,
                data_disk_gib: None,
                ip: None,
                storage: Storage::Local,
                image: "img".into(),
                placement: Default::default(),
                user_data: None,
                meta_data: Default::default(),
                labels: Default::default(),
            })
        };
        let (v1, v2) = (mk("one"), mk("two"));
        let (id1, id2) = (v1.id, v2.id);
        let now = Utc::now();
        let vol = Volume {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "pv".into(),
            server: "http://abe".into(),
            traddr: "10.22.0.1".into(),
            port: "44002".into(),
            bytes: 1,
            perf: None,
            attach_to: Some(id1),
            attached: None,
            message: None,
            deleting: false,
            created: now,
            updated: now,
        };
        let vid = vol.id.clone();
        store
            .update(|s| {
                s.vms.insert(id1, v1);
                s.vms.insert(id2, v2);
                s.volumes.insert(vid.clone(), vol);
            })
            .await
            .unwrap();

        r.tick().await.unwrap(); // VMs placed + provisioned; volume plugged in the same tick
        {
            let s = store.read().await;
            assert_eq!(s.vms[&id1].phase, VmPhase::Running);
            let v = &s.volumes[&vid];
            assert_eq!(v.attached.as_ref().map(|a| a.vm), Some(id1));
            assert_eq!(v.attached.as_ref().unwrap().target, "vdc");
            assert_eq!(v.message, None);
            assert_eq!(
                fake.plugged.lock().unwrap().as_slice(),
                [("one".to_string(), vid.clone())]
            );
        }

        // Re-point at the other VM: unplug this tick, plug next.
        store
            .update(|s| s.volumes.get_mut(&vid).unwrap().attach_to = Some(id2))
            .await
            .unwrap();
        r.tick().await.unwrap();
        assert_eq!(store.read().await.volumes[&vid].attached, None);
        assert!(fake.plugged.lock().unwrap().is_empty());
        r.tick().await.unwrap();
        assert_eq!(
            store.read().await.volumes[&vid]
                .attached
                .as_ref()
                .map(|a| a.vm),
            Some(id2)
        );

        // The VM goes away: the attachment and the wish are dropped, the
        // volume itself stays.
        store
            .update(|s| s.vms.get_mut(&id2).unwrap().phase = VmPhase::Deleting)
            .await
            .unwrap();
        r.tick().await.unwrap();
        r.tick().await.unwrap();
        let s = store.read().await;
        assert!(!s.vms.contains_key(&id2));
        let v = &s.volumes[&vid];
        assert_eq!(v.attached, None);
        assert_eq!(v.attach_to, None);
        assert!(fake.plugged.lock().unwrap().is_empty());
    }
}
