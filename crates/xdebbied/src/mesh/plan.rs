// SPDX-License-Identifier: GPL-3.0-or-later
//! `diff(desired, observed) -> Vec<Action>`. Pure. Running it on the state
//! it just produced must return an empty plan (tested below).
//!
//! Output order follows section 6.5: deletes, (re)creates, link changes,
//! FDB deletes, FDB adds, bridge MTU. The firewall is handled by the
//! manager because it is not a per-device action.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    kernel::{Action, Link, Observed, PortFlags},
    model::*,
};

/// Peers excluded from planning this round (route mismatch, name conflict),
/// with the reason to record in status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    pub box_id: u8,
    pub reason: &'static str,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub excluded: Vec<Excluded>,
}

#[cfg(test)]
impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Bridge port flags for a mode (section 2.2).
pub fn port_flags(mode: &FdbMode) -> PortFlags {
    let learn = *mode == FdbMode::Learn;
    PortFlags {
        isolated: true,
        learning: learn,
        flood: learn,
        mcast_flood: true,
        bcast_flood: true,
        neigh_suppress: false,
    }
}

/// Inputs the planner needs beyond the spec and the kernel snapshot.
#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    pub spec: &'a MeshSpec,
    pub observed: &'a Observed,
    pub mtu: u16,
    /// Peers whose route does not go through the underlay (section 6.3).
    pub route_bad: &'a BTreeSet<u8>,
}

fn create_sequence(dev: &str, spec: &MeshSpec, p: &PeerSpec, mtu: u16, bridge: &str) -> Vec<Action> {
    vec![
        Action::CreateTunnel { dev: dev.into(), vni: spec.vni(), remote: p.remote, dstport: spec.dstport },
        Action::SetAlias { dev: dev.into() },
        Action::SetMtu { dev: dev.into(), mtu },
        Action::SetMaster { dev: dev.into(), bridge: bridge.into() },
        Action::SetPortFlags { dev: dev.into(), flags: port_flags(&spec.fdb) },
        Action::SetUp { dev: dev.into() },
    ]
}

fn needs_recreate(l: &Link, spec: &MeshSpec, p: &PeerSpec) -> bool {
    match l.geneve() {
        Some(g) => g.vni != spec.vni() || g.remote != Some(p.remote) || g.dstport != spec.dstport || g.ttl != 64 || !g.df_set,
        None => true, // mesh name + alias but not geneve: treat as broken, rebuild
    }
}

pub fn diff(input: PlanInput<'_>) -> Plan {
    let PlanInput { spec, observed, mtu, route_bad } = input;
    let bridge = spec.bridge();
    let flags = port_flags(&spec.fdb);

    let mut deletes = Vec::new();
    let mut creates = Vec::new();
    let mut link_changes = Vec::new();
    let mut fdb_dels = Vec::new();
    let mut fdb_adds = Vec::new();
    let mut excluded = Vec::new();

    // Devices that will exist (and be ours) after this plan — FDB targets.
    let mut active_devs: BTreeMap<String, u8> = BTreeMap::new();
    // Devices being deleted and created again: their FDB entries go with them.
    let mut recreated: BTreeSet<String> = BTreeSet::new();

    for p in &spec.peers {
        let dev = tunnel_name(p.box_id);
        let existing = observed.link(&dev);

        if let Some(l) = existing
            && !l.mesh_owned()
        {
            excluded.push(Excluded { box_id: p.box_id, reason: "NameConflict" });
            continue;
        }

        if route_bad.contains(&p.box_id) {
            excluded.push(Excluded { box_id: p.box_id, reason: "UnderlayRouteMismatch" });
            // Keep the device, but make sure it is down (section 6.3).
            if let Some(l) = existing
                && l.up
            {
                link_changes.push(Action::SetDown { dev: dev.clone() });
            }
            continue;
        }

        match existing {
            None => {
                creates.extend(create_sequence(&dev, spec, p, mtu, &bridge));
            }
            Some(l) if needs_recreate(l, spec, p) => {
                deletes.push(Action::DeleteTunnel { dev: dev.clone() });
                creates.extend(create_sequence(&dev, spec, p, mtu, &bridge));
                recreated.insert(dev.clone());
            }
            Some(l) => {
                if l.mtu != mtu as u32 {
                    link_changes.push(Action::SetMtu { dev: dev.clone(), mtu });
                }
                let port = observed.port(&dev);
                let enslaved = l.master.as_deref() == Some(bridge.as_str()) && port.is_some();
                if !enslaved {
                    link_changes.push(Action::SetMaster { dev: dev.clone(), bridge: bridge.clone() });
                    link_changes.push(Action::SetPortFlags { dev: dev.clone(), flags });
                } else if port.map(|pt| pt.flags != flags).unwrap_or(true) {
                    link_changes.push(Action::SetPortFlags { dev: dev.clone(), flags });
                }
                if !l.up {
                    link_changes.push(Action::SetUp { dev: dev.clone() });
                }
            }
        }
        active_devs.insert(dev, p.box_id);
    }

    // Mesh-owned tunnels with no desired peer.
    let wanted: BTreeSet<String> = spec.peers.iter().map(|p| tunnel_name(p.box_id)).collect();
    for l in observed.links.iter().filter(|l| l.mesh_owned()) {
        if !wanted.contains(&l.name) {
            deletes.push(Action::DeleteTunnel { dev: l.name.clone() });
        }
    }

    // FDB. Desired set: 17 MACs per active peer in derived mode; none in learn.
    let mut desired_fdb: BTreeSet<(String, String)> = BTreeSet::new();
    if spec.fdb == FdbMode::Derived {
        for (dev, box_id) in &active_devs {
            for mac in derived_macs(*box_id) {
                desired_fdb.insert((mac, dev.clone()));
            }
        }
    }
    let owned_existing: BTreeSet<String> =
        observed.links.iter().filter(|l| l.mesh_owned() && wanted.contains(&l.name)).map(|l| l.name.clone()).collect();
    let present: BTreeSet<(String, String)> = observed
        .fdb
        .iter()
        .filter(|e| e.is_static && owned_existing.contains(&e.dev) && !recreated.contains(&e.dev))
        .map(|e| (e.mac.clone(), e.dev.clone()))
        .collect();
    for (mac, dev) in present.difference(&desired_fdb) {
        fdb_dels.push(Action::FdbDel { mac: mac.clone(), dev: dev.clone(), bridge: bridge.clone() });
    }
    for (mac, dev) in desired_fdb.difference(&present) {
        fdb_adds.push(Action::FdbAdd { mac: mac.clone(), dev: dev.clone(), bridge: bridge.clone() });
    }

    // Bridge MTU and STP (always off on a mesh bridge; see Action docs).
    let mut bridge_mtu = Vec::new();
    if let Some(b) = observed.link(&bridge) {
        if b.mtu != mtu as u32 {
            bridge_mtu.push(Action::SetBridgeMtu { bridge: bridge.clone(), mtu });
        }
        if matches!(&b.kind, super::kernel::LinkKind::Bridge(bi) if bi.stp_enabled) {
            bridge_mtu.push(Action::SetBridgeStpOff { bridge: bridge.clone() });
        }
    }

    let mut actions = deletes;
    actions.extend(creates);
    actions.extend(link_changes);
    actions.extend(fdb_dels);
    actions.extend(fdb_adds);
    actions.extend(bridge_mtu);
    Plan { actions, excluded }
}

/// Teardown: every mesh-owned tunnel goes. Nothing else.
pub fn teardown(observed: &Observed) -> Vec<Action> {
    observed
        .links
        .iter()
        .filter(|l| l.mesh_owned())
        .map(|l| Action::DeleteTunnel { dev: l.name.clone() })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::mesh::kernel::{FakeNetOps, FdbEntry, GeneveInfo, LinkKind, NetOps};

    fn spec(n: u8) -> MeshSpec {
        MeshSpec {
            enabled: true,
            box_id: 0,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            peers: (1..=n)
                .map(|i| PeerSpec::new(i, Ipv4Addr::new(10, 22, i, 1)))
                .collect(),
            ..Default::default()
        }
    }
    fn fake() -> FakeNetOps {
        let f = FakeNetOps::new();
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        f
    }
    fn plan_for(f: &FakeNetOps, s: &MeshSpec) -> Plan {
        let obs = f.observe(&s.bridge(), None).unwrap();
        diff(PlanInput { spec: s, observed: &obs, mtu: 1370, route_bad: &BTreeSet::new() })
    }
    fn apply_all(f: &FakeNetOps, p: &Plan) {
        for a in &p.actions {
            f.apply(a).unwrap();
        }
    }
    fn converge(f: &FakeNetOps, s: &MeshSpec) -> Plan {
        let p = plan_for(f, s);
        apply_all(f, &p);
        p
    }
    fn count(p: &Plan, kind: &str) -> usize {
        p.actions.iter().filter(|a| a.kind() == kind).count()
    }

    #[test]
    fn empty_to_three_peers() {
        let f = fake();
        let s = spec(3);
        let p = plan_for(&f, &s);
        assert_eq!(count(&p, "create_tunnel"), 3);
        assert_eq!(count(&p, "fdb_add"), 51);
        assert_eq!(count(&p, "set_bridge_mtu"), 1);
        assert_eq!(count(&p, "delete_tunnel"), 0);
        // Sequence for the first peer.
        let kinds: Vec<_> = p.actions.iter().take(6).map(|a| a.kind()).collect();
        assert_eq!(kinds, ["create_tunnel", "set_alias", "set_mtu", "set_master", "set_port_flags", "set_up"]);
        // Derived mode: learning and flood off, isolated on.
        let Action::SetPortFlags { flags, .. } = &p.actions[4] else { panic!() };
        assert!(flags.isolated && !flags.learning && !flags.flood && flags.bcast_flood);
        // Ordering: all FDB adds come after link changes and before bridge MTU.
        let last = p.actions.last().unwrap();
        assert_eq!(last.kind(), "set_bridge_mtu");
    }

    #[test]
    fn idempotent() {
        let f = fake();
        let s = spec(3);
        converge(&f, &s);
        let again = plan_for(&f, &s);
        assert!(again.is_empty(), "second plan must be empty: {:?}", again.actions);
    }

    #[test]
    fn remote_change_recreates_only_that_peer() {
        let f = fake();
        let mut s = spec(3);
        converge(&f, &s);
        s.peers[1].remote = Ipv4Addr::new(10, 22, 2, 9);
        let p = plan_for(&f, &s);
        assert_eq!(count(&p, "delete_tunnel"), 1);
        assert_eq!(count(&p, "create_tunnel"), 1);
        assert_eq!(p.actions[0], Action::DeleteTunnel { dev: "gnv-b2".into() });
        assert!(p.actions.iter().all(|a| a.dev() == "gnv-b2"), "{:?}", p.actions);
        // 17 FDB entries re-added for gnv-b2 (its old ones vanish with the device).
        assert_eq!(count(&p, "fdb_add"), 17);
        assert_eq!(count(&p, "fdb_del"), 0);
        apply_all(&f, &p);
        assert!(plan_for(&f, &s).is_empty());
    }

    #[test]
    fn peer_removed_deletes_only_its_tunnel() {
        let f = fake();
        let mut s = spec(3);
        converge(&f, &s);
        let idx_b1 = f.snapshot().links["gnv-b1"].ifindex;
        s.peers.retain(|p| p.box_id != 3);
        let p = plan_for(&f, &s);
        assert_eq!(p.actions, vec![Action::DeleteTunnel { dev: "gnv-b3".into() }]);
        apply_all(&f, &p);
        assert!(plan_for(&f, &s).is_empty());
        assert_eq!(f.snapshot().links["gnv-b1"].ifindex, idx_b1);
        assert!(!f.snapshot().fdb.iter().any(|e| e.dev == "gnv-b3"));
    }

    #[test]
    fn foreign_link_is_a_name_conflict() {
        let f = fake();
        f.add_link(Link {
            ifindex: 0,
            name: "gnv-b1".into(),
            mtu: 1500,
            up: false,
            master: None,
            alias: None,
            address: None,
            kind: LinkKind::Geneve(GeneveInfo { vni: 23, remote: Some(Ipv4Addr::new(10, 22, 1, 1)), dstport: 6081, ttl: 64, df_set: true }),
        });
        let s = spec(2);
        let p = plan_for(&f, &s);
        assert_eq!(p.excluded, vec![Excluded { box_id: 1, reason: "NameConflict" }]);
        assert!(p.actions.iter().all(|a| a.dev() != "gnv-b1"), "{:?}", p.actions);
        assert_eq!(count(&p, "create_tunnel"), 1);
        assert_eq!(count(&p, "fdb_add"), 17);
    }

    #[test]
    fn derived_to_learn_removes_static_entries_and_changes_flags() {
        let f = fake();
        let mut s = spec(2);
        converge(&f, &s);
        s.fdb = FdbMode::Learn;
        let p = plan_for(&f, &s);
        assert_eq!(count(&p, "fdb_del"), 34);
        assert_eq!(count(&p, "set_port_flags"), 2);
        assert_eq!(count(&p, "delete_tunnel"), 0);
        assert_eq!(count(&p, "create_tunnel"), 0);
        apply_all(&f, &p);
        assert!(plan_for(&f, &s).is_empty());
        // Learned (non-static) entries are left alone in learn mode.
        f.state.lock().unwrap().fdb.push(FdbEntry { mac: "aa:bb:cc:dd:ee:ff".into(), dev: "gnv-b1".into(), is_static: false });
        assert!(plan_for(&f, &s).is_empty());
        // And back to derived re-adds the 34.
        s.fdb = FdbMode::Derived;
        let p = plan_for(&f, &s);
        assert_eq!(count(&p, "fdb_add"), 34);
        assert_eq!(count(&p, "fdb_del"), 0, "learned entries are not ours to delete");
    }

    #[test]
    fn mtu_change_touches_mtu_only() {
        let f = fake();
        let s = spec(3);
        converge(&f, &s);
        let obs = f.observe("br-int", None).unwrap();
        let p = diff(PlanInput { spec: &s, observed: &obs, mtu: 1300, route_bad: &BTreeSet::new() });
        assert_eq!(count(&p, "set_mtu"), 3);
        assert_eq!(count(&p, "set_bridge_mtu"), 1);
        assert_eq!(p.actions.len(), 4);
    }

    #[test]
    fn route_mismatch_sets_down_not_delete() {
        let f = fake();
        let s = spec(2);
        converge(&f, &s);
        let obs = f.observe("br-int", None).unwrap();
        let bad: BTreeSet<u8> = [2].into();
        let p = diff(PlanInput { spec: &s, observed: &obs, mtu: 1370, route_bad: &bad });
        assert_eq!(p.excluded, vec![Excluded { box_id: 2, reason: "UnderlayRouteMismatch" }]);
        assert!(p.actions.contains(&Action::SetDown { dev: "gnv-b2".into() }));
        assert_eq!(count(&p, "delete_tunnel"), 0);
        // Its 17 static entries are pruned while it is excluded...
        assert_eq!(count(&p, "fdb_del"), 17);
        apply_all(&f, &p);
        // ...and nothing more happens while the route stays bad.
        let obs = f.observe("br-int", None).unwrap();
        assert!(diff(PlanInput { spec: &s, observed: &obs, mtu: 1370, route_bad: &bad }).is_empty());
        // Route fixed: back up, entries back.
        let p = plan_for(&f, &s);
        assert!(p.actions.contains(&Action::SetUp { dev: "gnv-b2".into() }));
        assert_eq!(count(&p, "fdb_add"), 17);
    }

    #[test]
    fn stale_static_entry_on_live_tunnel_is_removed() {
        let f = fake();
        let s = spec(1);
        converge(&f, &s);
        f.state.lock().unwrap().fdb.push(FdbEntry { mac: "52:54:00:23:09:01".into(), dev: "gnv-b1".into(), is_static: true });
        let p = plan_for(&f, &s);
        assert_eq!(p.actions, vec![Action::FdbDel { mac: "52:54:00:23:09:01".into(), dev: "gnv-b1".into(), bridge: "br-int".into() }]);
    }

    #[test]
    fn detached_tunnel_is_re_enslaved() {
        let f = fake();
        let s = spec(1);
        converge(&f, &s);
        {
            let mut st = f.state.lock().unwrap();
            st.links.get_mut("gnv-b1").unwrap().master = None;
            st.ports.remove("gnv-b1");
        }
        let p = plan_for(&f, &s);
        assert_eq!(count(&p, "set_master"), 1);
        assert_eq!(count(&p, "set_port_flags"), 1);
        assert_eq!(count(&p, "create_tunnel"), 0);
    }

    #[test]
    fn disabled_teardown_only_touches_owned() {
        let f = fake();
        let s = spec(2);
        converge(&f, &s);
        f.add_link(Link {
            ifindex: 0,
            name: "gnv-b9".into(),
            mtu: 1500,
            up: true,
            master: Some("br-int".into()),
            alias: None,
            address: None,
            kind: LinkKind::Geneve(GeneveInfo { vni: 23, remote: Some(Ipv4Addr::new(10, 22, 9, 1)), dstport: 6081, ttl: 64, df_set: true }),
        });
        f.add_link(Link {
            ifindex: 0,
            name: "vnet3".into(),
            mtu: 1500,
            up: true,
            master: Some("br-int".into()),
            alias: None,
            address: None,
            kind: LinkKind::Other("tun".into()),
        });
        let obs = f.observe("br-int", None).unwrap();
        let t = teardown(&obs);
        assert_eq!(t.len(), 2);
        assert!(t.iter().all(|a| a.dev() == "gnv-b1" || a.dev() == "gnv-b2"));
    }
}
