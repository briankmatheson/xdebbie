// SPDX-License-Identifier: GPL-3.0-or-later
//! Placement: filter, then score. Deterministic for identical inputs.
//!
//! Filters (hard): health, cordon, pin, failure domain, control-plane
//! eligibility, labels, capacity (cpu / memory / disk), strict anti-affinity.
//! For `storage = abe` VMs the disk check is against the host's ABE
//! (`Host::abe_fit`: pool fill, provisioned bytes plus what is pending
//! against overcommit) instead of the libvirt pool, so such a VM never
//! lands on a host without an ABE or with a full one.
//!
//! Scores (soft, higher wins):
//!   1. anti-affinity: fewest group peers on the host, then in its failure domain
//!   2. policy: `Spread` prefers the emptiest host, `Pack` the fullest
//!      (for ABE VMs the ABE pool counts as a resource too)
//!   3. name, as a stable tie-breaker

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{DiskRole, Host, Storage, Vm, VmSpec};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlacementPolicy {
    /// Least-allocated first: best for small clouds that want headroom per box.
    #[default]
    Spread,
    /// Most-allocated first: keep whole boxes free (power them off, upgrade them).
    Pack,
}

#[derive(Debug, Error, PartialEq)]
pub enum PlacementError {
    #[error("no host satisfies the request: {0}")]
    NoFit(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub host: String,
    /// Peers of the same anti-affinity group on this host.
    pub host_peers: usize,
    /// Peers of the same anti-affinity group in this host's failure domain.
    pub fd_peers: usize,
    /// Fraction of the dominant resource in use after placement (0..=1).
    pub utilization: f64,
}

#[derive(Default, Clone, Copy)]
struct Used {
    cpus: u64,
    mem: u64,
    disk: u64,
}

/// Bytes a `storage = abe` VM asks its ABE for, by role.
pub fn abe_roles(spec: &VmSpec) -> Vec<(DiskRole, u64)> {
    let mut v = vec![(DiskRole::Root, spec.disk_gib << 30)];
    if let Some(g) = spec.data_disk_gib {
        v.push((DiskRole::Data, g << 30));
    }
    v
}

/// Total bytes a `storage = abe` VM asks for.
pub fn abe_request(spec: &VmSpec) -> u64 {
    abe_roles(spec).iter().map(|(_, b)| b).sum()
}

/// ABE bytes promised to placed VMs but not yet visible in their host's
/// `abe_capacity`: for every ABE VM that counts against capacity, the
/// roles **not yet** in `vm.disks`. Disks that exist are already in the
/// ABE's `provisioned_bytes`, so counting them again would double-count.
/// Keyed by host name; only hosts in `hosts` appear.
pub fn abe_pending(hosts: &[Host], vms: &[Vm]) -> BTreeMap<String, u64> {
    let mut m: BTreeMap<String, u64> = hosts.iter().map(|h| (h.name.clone(), 0)).collect();
    for vm in vms
        .iter()
        .filter(|v| v.spec.storage == Storage::Abe && v.consumes_capacity())
    {
        let Some(h) = vm.host.as_deref() else {
            continue;
        };
        let Some(slot) = m.get_mut(h) else { continue };
        for (role, bytes) in abe_roles(&vm.spec) {
            if !vm.disks.iter().any(|d| d.role == role) {
                *slot += bytes;
            }
        }
    }
    m
}

/// Choose a host for `spec`. `vms` is every VM currently known (the
/// candidate itself may be included with `exclude` set to its id, which is
/// what migration uses). `abe_inflight` is bytes per host reserved by
/// standalone volume creates still in progress (the API's ledger); it is
/// added to the pending bytes computed from `vms`. Returns the full ranked
/// list; index 0 is the winner.
pub fn place(
    spec: &VmSpec,
    hosts: &[Host],
    vms: &[Vm],
    policy: PlacementPolicy,
    healthy: impl Fn(&Host) -> bool,
    exclude: Option<uuid::Uuid>,
    abe_inflight: &BTreeMap<String, u64>,
) -> Result<Vec<Scored>, PlacementError> {
    let abe = spec.storage == Storage::Abe;
    let mut pending = abe_pending(hosts, vms);
    if let Some(x) = exclude {
        // The excluded VM's own promises must not block its new host.
        if let Some(v) = vms
            .iter()
            .find(|v| v.id == x && v.spec.storage == Storage::Abe && v.consumes_capacity())
            && let Some(h) = v.host.as_deref()
            && let Some(slot) = pending.get_mut(h)
        {
            let own: u64 = abe_roles(&v.spec)
                .iter()
                .filter(|(r, _)| !v.disks.iter().any(|d| d.role == *r))
                .map(|(_, b)| b)
                .sum();
            *slot = slot.saturating_sub(own);
        }
    }
    for (h, b) in abe_inflight {
        if let Some(slot) = pending.get_mut(h) {
            *slot += b;
        }
    }
    let mut used: BTreeMap<&str, Used> = BTreeMap::new();
    let mut host_peers: BTreeMap<&str, usize> = BTreeMap::new();
    let fd_of: BTreeMap<&str, &str> = hosts
        .iter()
        .map(|h| (h.name.as_str(), h.failure_domain.as_str()))
        .collect();
    let mut fd_peers: BTreeMap<&str, usize> = BTreeMap::new();
    let group = spec.placement.anti_affinity_group.as_deref();

    for vm in vms
        .iter()
        .filter(|v| Some(v.id) != exclude && v.consumes_capacity())
    {
        let h = vm.host.as_deref().unwrap();
        let u = used.entry(h).or_default();
        u.cpus += vm.spec.vcpus as u64;
        u.mem += vm.spec.memory_mib;
        u.disk += vm.spec.disk_gib;
        if group.is_some() && vm.spec.placement.anti_affinity_group.as_deref() == group {
            *host_peers.entry(h).or_default() += 1;
            if let Some(fd) = fd_of.get(h) {
                *fd_peers.entry(fd).or_default() += 1;
            }
        }
    }

    let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut reject = |r: &'static str| {
        *reasons.entry(r).or_default() += 1;
    };
    let p = &spec.placement;
    let mut out = Vec::new();

    for h in hosts {
        if p.host.as_deref().is_some_and(|n| n != h.name) {
            reject("pinned elsewhere");
            continue;
        }
        if h.cordoned {
            reject("cordoned");
            continue;
        }
        if !healthy(h) {
            reject("unhealthy");
            continue;
        }
        if p.failure_domain
            .as_deref()
            .is_some_and(|fd| fd != h.failure_domain)
        {
            reject("failure domain");
            continue;
        }
        if p.control_plane && !h.control_plane_eligible {
            reject("not control-plane eligible");
            continue;
        }
        if !p
            .required_labels
            .iter()
            .all(|(k, v)| h.labels.get(k) == Some(v))
        {
            reject("labels");
            continue;
        }
        let Some(cap) = h.allocatable() else {
            reject("capacity unknown");
            continue;
        };
        let u = used.get(h.name.as_str()).copied().unwrap_or_default();
        let (c, m, d) = (
            u.cpus + spec.vcpus as u64,
            u.mem + spec.memory_mib,
            u.disk + spec.disk_gib,
        );
        if c > cap.cpus as u64 {
            reject("cpu");
            continue;
        }
        if m > cap.memory_mib {
            reject("memory");
            continue;
        }
        let mut abe_util = 0.0;
        if abe {
            // Only the seed ISO lands in the libvirt pool; the disks come
            // from the host's ABE, which has its own accounting.
            let pend = pending.get(h.name.as_str()).copied().unwrap_or(0);
            let req = abe_request(spec);
            if let Err(r) = h.abe_fit(pend, req) {
                reject(r);
                continue;
            }
            abe_util = h.abe_used_fraction(pend, req).unwrap_or(0.0);
        } else if spec.disk_gib > cap.disk_free_gib {
            // disk_free_gib is *free* space reported by the pool, so compare
            // the new disk only; thin qcow2 overlays make this conservative.
            reject("disk");
            continue;
        }
        let _ = d;
        let hp = host_peers.get(h.name.as_str()).copied().unwrap_or(0);
        if p.strict_anti_affinity && hp > 0 {
            reject("strict anti-affinity");
            continue;
        }
        let util = (c as f64 / cap.cpus.max(1) as f64)
            .max(m as f64 / cap.memory_mib.max(1) as f64)
            .max(abe_util);
        out.push(Scored {
            host: h.name.clone(),
            host_peers: hp,
            fd_peers: fd_peers
                .get(h.failure_domain.as_str())
                .copied()
                .unwrap_or(0),
            utilization: util,
        });
    }

    if out.is_empty() {
        let why = reasons
            .iter()
            .map(|(r, n)| format!("{n}×{r}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PlacementError::NoFit(if why.is_empty() {
            "no hosts registered".into()
        } else {
            why
        }));
    }

    out.sort_by(|a, b| {
        a.host_peers
            .cmp(&b.host_peers)
            .then(a.fd_peers.cmp(&b.fd_peers))
            .then_with(|| {
                let o = a.utilization.partial_cmp(&b.utilization).unwrap();
                match policy {
                    PlacementPolicy::Spread => o,
                    PlacementPolicy::Pack => o.reverse(),
                }
            })
            .then(a.host.cmp(&b.host))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;

    fn host(name: &str, fd: &str, cpus: u32, mem_gib: u64) -> Host {
        Host {
            name: name.into(),
            uri: format!("qemu+ssh://{name}/system"),
            failure_domain: fd.into(),
            control_plane_eligible: true,
            labels: Default::default(),
            storage_pool: "default".into(),
            bridge: "br0".into(),
            cpu_overcommit: 1.0,
            abe: None,
            abe_capacity: None,
            abe_overcommit: 1.0,
            abe_max_data_percent: 85.0,
            abe_max_metadata_percent: 80.0,
            mem_overcommit: 1.0,
            reserved_memory_mib: 0,
            reserved_cpus: 0,
            cordoned: false,
            capacity: Some(Capacity {
                cpus,
                memory_mib: mem_gib * 1024,
                disk_free_gib: 1000,
            }),
            last_seen: Some(Utc::now()),
        }
    }
    fn spec(name: &str, cpus: u32, mem_gib: u64, group: Option<&str>) -> VmSpec {
        VmSpec {
            name: name.into(),
            vcpus: cpus,
            memory_mib: mem_gib * 1024,
            disk_gib: 20,
            data_disk_gib: None,
            ip: None,
            storage: Default::default(),
            image: "debian-13".into(),
            placement: Placement {
                anti_affinity_group: group.map(Into::into),
                ..Default::default()
            },
            user_data: None,
            meta_data: Default::default(),
            labels: Default::default(),
        }
    }
    fn running(spec: VmSpec, host: &str) -> Vm {
        Vm {
            id: uuid::Uuid::new_v4(),
            spec,
            phase: VmPhase::Running,
            power: PowerIntent::On,
            host: Some(host.into()),
            addresses: vec![],
            ip: None,
            disks: Vec::new(),
            message: None,
            migrate_to: None,
            reboot_generation: 0,
            observed_reboot_generation: 0,
            created: Utc::now(),
            updated: Utc::now(),
        }
    }
    fn ok(_: &Host) -> bool {
        true
    }

    #[test]
    fn spread_prefers_emptiest() {
        let hosts = vec![host("a", "r1", 16, 64), host("b", "r1", 16, 64)];
        let vms = vec![running(spec("x", 8, 8, None), "a")];
        let r = place(
            &spec("y", 2, 2, None),
            &hosts,
            &vms,
            PlacementPolicy::Spread,
            ok,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(r[0].host, "b");
    }

    #[test]
    fn pack_prefers_fullest() {
        let hosts = vec![host("a", "r1", 16, 64), host("b", "r1", 16, 64)];
        let vms = vec![running(spec("x", 8, 8, None), "a")];
        let r = place(
            &spec("y", 2, 2, None),
            &hosts,
            &vms,
            PlacementPolicy::Pack,
            ok,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(r[0].host, "a");
    }

    #[test]
    fn anti_affinity_beats_utilization_and_spreads_fds() {
        let hosts = vec![
            host("a", "r1", 64, 256),
            host("b", "r1", 8, 16),
            host("c", "r2", 8, 16),
        ];
        let vms = vec![running(spec("cp0", 2, 4, Some("cp")), "a")];
        let r = place(
            &spec("cp1", 2, 4, Some("cp")),
            &hosts,
            &vms,
            PlacementPolicy::Spread,
            ok,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        // b and c both have 0 host peers, but c is in a different failure domain.
        assert_eq!(r[0].host, "c");
    }

    #[test]
    fn strict_anti_affinity_rejects() {
        let hosts = vec![host("a", "r1", 64, 256)];
        let vms = vec![running(spec("cp0", 2, 4, Some("cp")), "a")];
        let mut s = spec("cp1", 2, 4, Some("cp"));
        s.placement.strict_anti_affinity = true;
        assert!(
            place(
                &s,
                &hosts,
                &vms,
                PlacementPolicy::Spread,
                ok,
                None,
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn capacity_and_cordon_filters() {
        let mut a = host("a", "r1", 4, 8);
        let mut b = host("b", "r1", 64, 256);
        b.cordoned = true;
        let r = place(
            &spec("big", 8, 4, None),
            &[a.clone(), b.clone()],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &BTreeMap::new(),
        );
        assert!(matches!(r, Err(PlacementError::NoFit(_))));
        a.capacity.as_mut().unwrap().cpus = 8;
        let r = place(
            &spec("big", 8, 4, None),
            &[a, b],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(r[0].host, "a");
    }

    #[test]
    fn failure_domain_is_hard() {
        let hosts = vec![host("a", "r1", 64, 256), host("b", "r2", 8, 16)];
        let mut s = spec("w", 2, 2, None);
        s.placement.failure_domain = Some("r2".into());
        assert_eq!(
            place(
                &s,
                &hosts,
                &[],
                PlacementPolicy::Spread,
                ok,
                None,
                &BTreeMap::new()
            )
            .unwrap()[0]
                .host,
            "b"
        );
    }

    #[test]
    fn reservations_and_overcommit() {
        let mut h = host("a", "r1", 8, 32);
        h.reserved_cpus = 2;
        h.cpu_overcommit = 4.0;
        h.reserved_memory_mib = 8192;
        let a = h.allocatable().unwrap();
        assert_eq!(a.cpus, 24);
        assert_eq!(a.memory_mib, 24 * 1024);
    }

    #[test]
    fn migration_excludes_self() {
        let hosts = vec![host("a", "r1", 4, 8), host("b", "r1", 4, 8)];
        let vm = running(spec("x", 4, 8, None), "a");
        let mut s = vm.spec.clone();
        s.placement.host = Some("a".into());
        // Without exclusion the VM's own footprint blocks host a.
        assert!(
            place(
                &s,
                &hosts,
                std::slice::from_ref(&vm),
                PlacementPolicy::Spread,
                ok,
                None,
                &BTreeMap::new(),
            )
            .is_err()
        );
        assert!(
            place(
                &s,
                &hosts,
                std::slice::from_ref(&vm),
                PlacementPolicy::Spread,
                ok,
                Some(vm.id),
                &BTreeMap::new(),
            )
            .is_ok()
        );
    }

    // ---- storage = abe ----

    fn abe_cap(thin: bool, bytes_gib: u64, provisioned_gib: u64) -> AbeCapacity {
        AbeCapacity {
            thin,
            bytes: bytes_gib << 30,
            used_bytes: provisioned_gib << 30,
            free_bytes: (bytes_gib - provisioned_gib.min(bytes_gib)) << 30,
            provisioned_bytes: provisioned_gib << 30,
            data_percent: 10.0,
            metadata_percent: Some(5.0),
            tls: false,

            seen: Utc::now(),
        }
    }
    fn abe_host(name: &str, cap: Option<AbeCapacity>) -> Host {
        let mut h = host(name, "r1", 16, 64);
        h.abe = Some(format!("http://{name}:80"));
        h.abe_capacity = cap;
        // the libvirt pool is nearly full; it must not matter for abe VMs
        h.capacity.as_mut().unwrap().disk_free_gib = 1;
        h
    }
    fn abe_spec(name: &str, disk_gib: u64, data_gib: Option<u64>) -> VmSpec {
        let mut s = spec(name, 1, 1, None);
        s.storage = Storage::Abe;
        s.disk_gib = disk_gib;
        s.data_disk_gib = data_gib;
        s
    }
    fn none() -> BTreeMap<String, u64> {
        BTreeMap::new()
    }

    #[test]
    fn abe_vm_needs_an_abe_host_with_known_capacity() {
        let plain = host("plain", "r1", 16, 64); // no abe, 1000 GiB pool free
        let unknown = abe_host("unknown", None);
        let r = place(
            &abe_spec("v", 10, None),
            &[plain, unknown],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        );
        assert_eq!(
            r,
            Err(PlacementError::NoFit(
                "1×abe capacity unknown, 1×no abe".into()
            ))
        );
    }

    #[test]
    fn abe_vm_ignores_the_libvirt_pool_and_fits_by_abe_capacity() {
        let thin = abe_host("thin", Some(abe_cap(true, 100, 60)));
        let thick = abe_host("thick", Some(abe_cap(false, 100, 60)));
        // 10 GiB root + 20 GiB data = 30 GiB fits both (40 left)
        let r = place(
            &abe_spec("v", 10, Some(20)),
            &[thin.clone(), thick.clone()],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        // 50 GiB fits neither
        let r = place(
            &abe_spec("v", 50, None),
            &[thin.clone(), thick.clone()],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        );
        assert_eq!(r, Err(PlacementError::NoFit("2×abe capacity".into())));
        // overcommit opens the thin host only
        let mut t = thin.clone();
        t.abe_overcommit = 2.0;
        let r = place(
            &abe_spec("v", 50, None),
            &[t, thick],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        )
        .unwrap();
        assert_eq!(
            r.iter().map(|s| s.host.as_str()).collect::<Vec<_>>(),
            ["thin"]
        );
    }

    #[test]
    fn pending_disks_and_the_ledger_count_until_they_exist() {
        let h = abe_host("a", Some(abe_cap(true, 100, 60)));
        // a VM placed on `a` whose disks are not made yet promises 30 GiB
        let mut v = running(abe_spec("w", 10, Some(20)), "a");
        v.phase = VmPhase::Provisioning;
        let vms = vec![v.clone()];
        assert_eq!(abe_pending(std::slice::from_ref(&h), &vms)["a"], 30 << 30);
        assert!(
            place(
                &abe_spec("v", 11, None),
                std::slice::from_ref(&h),
                &vms,
                PlacementPolicy::Spread,
                ok,
                None,
                &none()
            )
            .is_err()
        );
        assert!(
            place(
                &abe_spec("v", 10, None),
                std::slice::from_ref(&h),
                &vms,
                PlacementPolicy::Spread,
                ok,
                None,
                &none()
            )
            .is_ok()
        );
        // once the root disk exists only the data disk is pending
        v.disks.push(Disk {
            role: DiskRole::Root,
            server: "http://a:80".into(),
            traddr: "10.22.1.1".into(),
            port: "44001".into(),
            id: "x".into(),
            bytes: 10 << 30,
            populated: true,
            origin: None,
        });
        assert_eq!(
            abe_pending(std::slice::from_ref(&h), std::slice::from_ref(&v))["a"],
            20 << 30
        );
        // the ledger (volumes being created) adds to pending
        let ledger: BTreeMap<String, u64> = [("a".to_string(), 15u64 << 30)].into();
        assert!(
            place(
                &abe_spec("v", 10, None),
                std::slice::from_ref(&h),
                std::slice::from_ref(&v),
                PlacementPolicy::Spread,
                ok,
                None,
                &ledger
            )
            .is_err()
        );
        let ledger: BTreeMap<String, u64> = [("a".to_string(), 10u64 << 30)].into();
        assert!(
            place(
                &abe_spec("v", 10, None),
                std::slice::from_ref(&h),
                std::slice::from_ref(&v),
                PlacementPolicy::Spread,
                ok,
                None,
                &ledger
            )
            .is_ok()
        );
    }

    #[test]
    fn spread_prefers_the_emptier_abe_pool() {
        let a = abe_host("a", Some(abe_cap(true, 100, 70)));
        let b = abe_host("b", Some(abe_cap(true, 100, 20)));
        let r = place(
            &abe_spec("v", 10, None),
            &[a.clone(), b.clone()],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        )
        .unwrap();
        assert_eq!(r[0].host, "b");
        assert!(r[0].utilization < r[1].utilization);
        let r = place(
            &abe_spec("v", 10, None),
            &[a, b],
            &[],
            PlacementPolicy::Pack,
            ok,
            None,
            &none(),
        )
        .unwrap();
        assert_eq!(r[0].host, "a");
    }

    #[test]
    fn a_full_pool_is_rejected_even_with_promises_to_spare() {
        let mut h = abe_host("a", Some(abe_cap(true, 100, 10)));
        h.abe_capacity.as_mut().unwrap().data_percent = 90.0;
        let r = place(
            &abe_spec("v", 1, None),
            &[h],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        );
        assert_eq!(r, Err(PlacementError::NoFit("1×abe pool full".into())));
    }

    #[test]
    fn local_vms_still_use_the_libvirt_pool() {
        let h = abe_host("a", Some(abe_cap(true, 100, 0))); // pool: 1 GiB free
        let r = place(
            &spec("v", 1, 1, None),
            &[h],
            &[],
            PlacementPolicy::Spread,
            ok,
            None,
            &none(),
        );
        assert_eq!(r, Err(PlacementError::NoFit("1×disk".into())));
    }
}
