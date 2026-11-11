// SPDX-License-Identifier: GPL-3.0-or-later
//! The control loop. Each tick: refresh host inventory, place pending VMs
//! one at a time (so each decision sees the previous one), then drive every
//! other VM one step toward its desired state, concurrently.
//!
//! The reconciler never holds the store lock across hypervisor I/O. It
//! computes a `Patch` from a snapshot and merges it, so API writes made in
//! the meantime (notably delete requests) are never lost.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use futures::future::join_all;
use xdebbie_core::{Address, Host, PowerIntent, Vm, VmPhase, place};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::Config,
    libvirt::{DomState, Hypervisor},
    store::{State, Store},
};

pub struct Reconciler<H> {
    pub store: Store,
    pub hv: Arc<H>,
    pub cfg: Arc<Config>,
    /// Lets the mesh name VMs (not `vnetN`) in `vms_mtu_mismatch`.
    pub mesh: Option<Arc<crate::mesh::MeshManager>>,
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
        vm.updated = Utc::now();
    }
}

impl<H: Hypervisor> Reconciler<H> {
    pub fn healthy(&self, h: &Host) -> bool {
        h.is_healthy(Utc::now(), self.cfg.host_stale_secs)
    }

    pub async fn run(self) {
        let mut t = tokio::time::interval(Duration::from_secs(self.cfg.reconcile_interval_secs));
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            if let Err(e) = self.tick().await {
                warn!(error = %e, "reconcile tick failed");
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
            .await
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
        let res = join_all(
            hosts
                .iter()
                .map(|h| async { (h.name.clone(), self.hv.inventory(h).await) }),
        )
        .await;
        self.store
            .update(|s| {
                for (name, r) in res {
                    let Some(h) = s.hosts.get_mut(&name) else {
                        continue;
                    };
                    match r {
                        Ok(c) => {
                            h.capacity = Some(c);
                            h.last_seen = Some(Utc::now());
                        }
                        Err(e) => warn!(host = %name, error = %e, "inventory failed"),
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
            self.store
                .update(|s| {
                    let hosts: Vec<Host> = s.hosts.values().cloned().collect();
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
                if let Err(e) = self.hv.provision(h, vm).await {
                    return Patch::msg(e.to_string());
                }
                if vm.power == PowerIntent::On
                    && let Err(e) = self.hv.start(h, vm).await
                {
                    return Patch::msg(e.to_string());
                }
                info!(vm = %vm.spec.name, host = %h.name, "provisioned");
                Patch {
                    phase: Some(if vm.power == PowerIntent::On {
                        VmPhase::Running
                    } else {
                        VmPhase::Stopped
                    }),
                    message: Some(None),
                    observed_reboot_generation: Some(vm.reboot_generation),
                    ..Default::default()
                }
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
            return Patch::msg(e.to_string());
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
                a.insert(0, Address { kind: "InternalIP".into(), address: ip.to_string() });
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
    use xdebbie_core::{Capacity, Placement, VmSpec};
    use std::{collections::BTreeMap, sync::Mutex};

    #[derive(Default)]
    struct Fake {
        doms: Mutex<BTreeMap<(String, String), DomState>>,
    }
    impl Hypervisor for Fake {
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
            Ok(())
        }
        async fn tap_owners(&self, _: &Host) -> anyhow::Result<std::collections::HashMap<String, String>> {
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
}
