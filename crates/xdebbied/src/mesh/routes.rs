// SPDX-License-Identifier: GPL-3.0-or-later
//! Routes back to xdj clients that are not this box's (docs/mesh.md,
//! "Clients behind another box").
//!
//! A client has one WireGuard link, to its hub. It reaches every box's VMs
//! through the hub and the overlay bridge, but a VM on another box answers
//! through *its* box, whose underlay route for `10.22/16` points into
//! WireGuard, where no peer carries the client's `10.22.<c>.1/32`, so the
//! reply is dropped (2026-11-26: gram could not reach eyrie-4 on box 5).
//!
//! A box with no clients of its own and at least one peer therefore sends
//! the underlay through its hub's bridge address, `10.23.<hub>.1`, the way
//! the request came. The hub is the lowest-numbered peer (box 0, the
//! founding box, where `xdctl mesh client add` is run). Peers keep their
//! own `/32` into WireGuard, so Geneve and probes are untouched:
//!
//! | route | why |
//! |---|---|
//! | `10.22.<peer>.1/32 dev <underlay>` | each peer's underlay stays in WireGuard |
//! | `10.22.0.0/17`, `10.22.128.0/17 via 10.23.<hub>.1 dev <bridge>` | everything else (clients) via the hub; two halves beat the kernel's `/16` |
//!
//! Every route carries `proto 230`, which is how ours are found again:
//! routes with it that are no longer wanted are deleted.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use super::model::{MeshSpec, bridge_addr, underlay_addr};

/// `ip route ... proto 230`: marks the routes this module owns.
pub const ROUTE_PROTO: &str = "230";

/// One route we own. `dst` is always `a.b.c.d/n`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostRoute {
    pub dst: String,
    pub via: Option<Ipv4Addr>,
    pub dev: String,
}

/// The routes this box should have (module docs). Empty when it is a hub
/// (has clients), has no peers, or does not manage WireGuard.
pub fn wanted(spec: &MeshSpec) -> Vec<HostRoute> {
    let Some(hub) = spec.peers.iter().min_by_key(|p| p.box_id) else {
        return vec![];
    };
    if spec.wireguard.is_none() || !spec.clients.is_empty() {
        return vec![];
    }
    let mut out: Vec<HostRoute> = spec
        .peers
        .iter()
        .map(|p| HostRoute {
            dst: format!("{}/32", p.remote),
            via: None,
            dev: spec.underlay.clone(),
        })
        .collect();
    out.extend(halves().into_iter().map(|dst| HostRoute {
        dst,
        // the hub's bridge address, never its probe address: with
        // `fdb = learn` that is the underlay 10.22.<hub>.1, not on the bridge
        // (0.22.1 on box 5: the /17s were refused, reconcile stuck)
        via: Some(bridge_addr(hub.box_id)),
        dev: spec.bridge(),
    }));
    out.sort();
    out
}

/// The underlay /16 as two /17s.
fn halves() -> [String; 2] {
    let net = u32::from(underlay_addr(0)) & 0xffff_0000;
    [
        format!("{}/17", Ipv4Addr::from(net)),
        format!("{}/17", Ipv4Addr::from(net | 0x8000)),
    ]
}

/// `(to_set, to_delete)`: wanted routes missing or different, and ours
/// whose destination is no longer wanted. Pure.
pub fn diff(wanted: &[HostRoute], have: &[HostRoute]) -> (Vec<HostRoute>, Vec<HostRoute>) {
    let have_set: BTreeSet<&HostRoute> = have.iter().collect();
    let want_dst: BTreeSet<&str> = wanted.iter().map(|r| r.dst.as_str()).collect();
    let set = wanted
        .iter()
        .filter(|r| !have_set.contains(r))
        .cloned()
        .collect();
    let del = have
        .iter()
        .filter(|r| !want_dst.contains(r.dst.as_str()))
        .cloned()
        .collect();
    (set, del)
}

/// Parse `ip -j route show proto 230`. A host route prints without `/32`.
pub fn parse(json: &str) -> anyhow::Result<Vec<HostRoute>> {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(if json.trim().is_empty() { "[]" } else { json })?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let dst = r.get("dst")?.as_str()?;
            Some(HostRoute {
                dst: if dst.contains('/') {
                    dst.to_owned()
                } else {
                    format!("{dst}/32")
                },
                via: r
                    .get("gateway")
                    .and_then(|g| g.as_str())
                    .and_then(|g| g.parse().ok()),
                dev: r.get("dev")?.as_str()?.to_owned(),
            })
        })
        .collect())
}

/// `ip route replace` arguments for a route.
pub fn replace_args(r: &HostRoute) -> Vec<String> {
    let mut a = vec!["route".to_string(), "replace".into(), r.dst.clone()];
    if let Some(v) = r.via {
        a.extend(["via".into(), v.to_string()]);
    }
    a.extend([
        "dev".into(),
        r.dev.clone(),
        "proto".into(),
        ROUTE_PROTO.into(),
    ]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::model::{ClientSpec, FdbMode, PeerSpec, WireguardSpec};

    fn box5() -> MeshSpec {
        MeshSpec {
            box_id: 5,
            underlay: "wg-flock".into(),
            wireguard: Some(WireguardSpec::default()),
            peers: vec![PeerSpec::new(0, underlay_addr(0))],
            ..Default::default()
        }
    }

    fn r(dst: &str, via: Option<&str>, dev: &str) -> HostRoute {
        HostRoute {
            dst: dst.into(),
            via: via.map(|v| v.parse().unwrap()),
            dev: dev.into(),
        }
    }

    #[test]
    fn a_box_without_clients_routes_the_underlay_via_its_hub() {
        let br = box5().bridge();
        assert_eq!(
            wanted(&box5()),
            vec![
                r("10.22.0.0/17", Some("10.23.0.1"), &br),
                r("10.22.0.1/32", None, "wg-flock"),
                r("10.22.128.0/17", Some("10.23.0.1"), &br),
            ]
        );
    }

    #[test]
    fn a_hub_no_peers_or_unmanaged_wireguard_wants_nothing() {
        let mut hub = box5();
        hub.clients = vec![ClientSpec {
            box_id: 1,
            pubkey: "k".into(),
            name: None,
            endpoint: None,
        }];
        assert!(wanted(&hub).is_empty());
        assert!(
            wanted(&MeshSpec {
                peers: vec![],
                ..box5()
            })
            .is_empty()
        );
        assert!(
            wanted(&MeshSpec {
                wireguard: None,
                ..box5()
            })
            .is_empty()
        );
    }

    #[test]
    fn the_gateway_is_the_hubs_bridge_address_in_every_fdb_mode() {
        for fdb in [FdbMode::Learn, FdbMode::Derived] {
            let s = MeshSpec {
                fdb: fdb.clone(),
                ..box5()
            };
            let vias: Vec<_> = wanted(&s).iter().filter_map(|x| x.via).collect();
            assert_eq!(vias, vec![Ipv4Addr::new(10, 23, 0, 1); 2], "{fdb:?}");
        }
    }

    #[test]
    fn the_lowest_peer_is_the_hub() {
        let mut s = box5();
        s.peers.insert(0, PeerSpec::new(3, underlay_addr(3)));
        assert!(
            wanted(&s)
                .iter()
                .all(|x| x.via.is_none_or(|v| v == Ipv4Addr::new(10, 23, 0, 1)))
        );
    }

    #[test]
    fn diff_sets_missing_and_deletes_unwanted_dsts() {
        let want = wanted(&box5());
        let stale = r("10.22.9.1/32", None, "wg-flock");
        let moved = r("10.22.0.0/17", Some("10.23.9.1"), "x");
        let (set, del) = diff(&want, &[stale.clone(), moved, want[1].clone()]);
        assert_eq!(set, vec![want[0].clone(), want[2].clone()]);
        assert_eq!(del, vec![stale]);
        assert_eq!(diff(&want, &want), (vec![], vec![]));
    }

    #[test]
    fn parse_reads_ip_json_and_adds_the_host_prefix() {
        let j = r#"[{"dst":"10.22.0.0/17","gateway":"10.23.0.1","dev":"virbr-xdebbie","protocol":"230","flags":[]},
                   {"dst":"10.22.0.1","dev":"wg-flock","protocol":"230","scope":"link","flags":[]}]"#;
        assert_eq!(
            parse(j).unwrap(),
            vec![
                r("10.22.0.0/17", Some("10.23.0.1"), "virbr-xdebbie"),
                r("10.22.0.1/32", None, "wg-flock")
            ]
        );
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn replace_args_carry_the_proto() {
        assert_eq!(
            replace_args(&r("10.22.0.0/17", Some("10.23.0.1"), "br")).join(" "),
            "route replace 10.22.0.0/17 via 10.23.0.1 dev br proto 230"
        );
    }
}
