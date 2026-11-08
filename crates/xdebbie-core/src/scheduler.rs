// SPDX-License-Identifier: GPL-3.0-or-later
//! Placement: filter, then score. Deterministic for identical inputs.
//!
//! Filters (hard): health, cordon, pin, failure domain, control-plane
//! eligibility, labels, capacity (cpu / memory / disk), strict anti-affinity.
//!
//! Scores (soft, higher wins):
//!   1. anti-affinity: fewest group peers on the host, then in its failure domain
//!   2. policy: `Spread` prefers the emptiest host, `Pack` the fullest
//!   3. name, as a stable tie-breaker

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{Host, Vm, VmSpec};

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

/// Choose a host for `spec`. `vms` is every VM currently known (the
/// candidate itself may be included with `exclude` set to its id, which is
/// what migration uses). Returns the full ranked list; index 0 is the winner.
pub fn place(
    spec: &VmSpec,
    hosts: &[Host],
    vms: &[Vm],
    policy: PlacementPolicy,
    healthy: impl Fn(&Host) -> bool,
    exclude: Option<uuid::Uuid>,
) -> Result<Vec<Scored>, PlacementError> {
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
        // disk_free_gib is *free* space reported by the pool, so compare the
        // new disk only; thin qcow2 overlays make this conservative.
        if spec.disk_gib > cap.disk_free_gib {
            reject("disk");
            continue;
        }
        let _ = d;
        let hp = host_peers.get(h.name.as_str()).copied().unwrap_or(0);
        if p.strict_anti_affinity && hp > 0 {
            reject("strict anti-affinity");
            continue;
        }
        let util = (c as f64 / cap.cpus.max(1) as f64).max(m as f64 / cap.memory_mib.max(1) as f64);
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
        assert!(place(&s, &hosts, &vms, PlacementPolicy::Spread, ok, None).is_err());
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
            place(&s, &hosts, &[], PlacementPolicy::Spread, ok, None).unwrap()[0].host,
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
                None
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
                Some(vm.id)
            )
            .is_ok()
        );
    }
}
