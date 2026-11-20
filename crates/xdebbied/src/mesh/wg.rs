// SPDX-License-Identifier: GPL-3.0-or-later
//! WireGuard underlay planner (0.5.0). Pure: observed kernel state in,
//! actions out, same discipline as `plan.rs` for Geneve.
//!
//! The mesh owns one WireGuard interface named by `spec.underlay`
//! (ownership = that name + kind wireguard + our ifalias). Its private key
//! lives in the state directory; the public key is what other boxes put in
//! their `peers[].pubkey`. Every peer with a `pubkey` becomes a WireGuard
//! peer with `allowed-ips = <remote>/32`, so the Geneve outer packets to
//! `10.22.<box>.1` are routed into the tunnel by cryptokey routing.
//! Clients (`spec.clients`, xdj) are WireGuard peers too, with
//! `allowed-ips = 10.22.<box>.1/32`, but get nothing else.

use std::{net::Ipv4Addr, path::Path};

use super::{
    kernel::{Action, LinkKind, Observed},
    model::{MeshSpec, OWNER_ALIAS},
};

/// What the planner needs besides the spec.
pub struct WgInput<'a> {
    pub spec: &'a MeshSpec,
    pub observed: &'a Observed,
    pub key_path: &'a Path,
    /// Public key derived from the key file (the daemon computes it once).
    pub pubkey: &'a str,
}

/// Outcome of looking at the underlay name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WgState {
    /// No link with that name: create it.
    Absent,
    /// Ours: reconcile.
    Owned,
    /// Something else has the name (foreign wireguard link, or a different
    /// kind of link). Never touched; blocking condition.
    Foreign(String),
}

pub fn classify(observed: &Observed, dev: &str) -> WgState {
    match observed.link(dev) {
        None => WgState::Absent,
        Some(l) if l.wg_owned(dev) => WgState::Owned,
        Some(l) => WgState::Foreign(match &l.kind {
            LinkKind::Wireguard => {
                format!("{dev} is a WireGuard link without the {OWNER_ALIAS} alias")
            }
            LinkKind::Other(k) => format!("{dev} exists but is a {k} link, not WireGuard"),
            LinkKind::Bridge(_) => format!("{dev} exists but is a bridge"),
            LinkKind::Geneve(_) => format!("{dev} exists but is a Geneve tunnel"),
            LinkKind::Unknown => format!("{dev} exists but is not a WireGuard link"),
        }),
    }
}

/// `allowed-ips` for a peer: its underlay address only.
pub fn allowed_ips_for(remote: Ipv4Addr) -> String {
    format!("{remote}/32")
}

/// Desired − observed. Returns nothing for a converged interface.
/// Caller must have checked `classify` != Foreign.
pub fn diff(input: WgInput<'_>) -> Vec<Action> {
    let spec = input.spec;
    let Some(w) = &spec.wireguard else {
        return vec![];
    };
    let dev = spec.underlay.clone();
    let observed = input.observed;
    let mut out = vec![];

    let link = observed.link(&dev).filter(|l| l.wg_owned(&dev));
    let fresh = link.is_none();
    if fresh {
        out.push(Action::WgCreate { dev: dev.clone() });
        out.push(Action::SetAlias { dev: dev.clone() });
    }

    // Interface identity: key + port.
    let iface = observed.wg.as_ref();
    let iface_ok = iface
        .map(|i| i.pubkey.as_deref() == Some(input.pubkey) && i.listen_port == w.listen_port)
        .unwrap_or(false);
    if fresh || !iface_ok {
        out.push(Action::WgSetIface {
            dev: dev.clone(),
            key_path: input.key_path.to_path_buf(),
            listen_port: w.listen_port,
        });
    }

    // Address.
    let addr = spec.wg_address().expect("wireguard set");
    let has_addr = observed
        .wg_addrs
        .iter()
        .any(|a| a.addr == addr && a.prefix == w.prefix);
    if fresh || !has_addr {
        out.push(Action::AddrAdd {
            dev: dev.clone(),
            addr,
            prefix: w.prefix,
        });
    }

    // MTU and admin state.
    if fresh || link.map(|l| l.mtu != w.mtu as u32).unwrap_or(true) {
        out.push(Action::SetMtu {
            dev: dev.clone(),
            mtu: w.mtu,
        });
    }
    if fresh || link.map(|l| !l.up).unwrap_or(true) {
        out.push(Action::SetUp { dev: dev.clone() });
    }

    // Peers.
    let desired = desired_peers(spec);
    for (pubkey, endpoint, allowed) in &desired {
        let cur = iface.and_then(|i| i.peer(pubkey));
        let same = cur
            .map(|c| {
                let ips_same = c.allowed_ips.len() == 1 && c.allowed_ips[0] == *allowed;
                let ka_same = c.keepalive == w.keepalive;
                ips_same
                    && ka_same
                    && endpoint_satisfied(endpoint.as_deref(), c.endpoint.as_deref())
            })
            .unwrap_or(false);
        if !same {
            out.push(Action::WgPeerSet {
                dev: dev.clone(),
                pubkey: pubkey.clone(),
                endpoint: endpoint.clone(),
                allowed_ips: allowed.clone(),
                keepalive: w.keepalive,
            });
        }
    }
    if let Some(i) = iface {
        for c in &i.peers {
            if !desired.iter().any(|(k, _, _)| *k == c.pubkey) {
                out.push(Action::WgPeerDel {
                    dev: dev.clone(),
                    pubkey: c.pubkey.clone(),
                });
            }
        }
    }
    out
}

/// The WireGuard peer set: every Geneve peer with a pubkey, then every
/// client (xdj) at `10.22.<box>.1/32`. Sorted by pubkey.
///
/// Validation keeps pubkeys and addresses unique; as a second line of
/// defence a client whose key or allowed-ips is already taken by a peer
/// (or an earlier client) is left out, because WireGuard would silently
/// move the address from one peer to the other.
fn desired_peers(spec: &MeshSpec) -> Vec<(String, Option<String>, String)> {
    let mut desired: Vec<(String, Option<String>, String)> = spec
        .peers
        .iter()
        .filter_map(|p| {
            p.pubkey
                .as_ref()
                .map(|k| (k.clone(), p.endpoint.clone(), allowed_ips_for(p.remote)))
        })
        .collect();
    for c in &spec.clients {
        let allowed = allowed_ips_for(c.addr());
        if desired
            .iter()
            .any(|(k, _, a)| *k == c.pubkey || *a == allowed)
        {
            continue;
        }
        desired.push((c.pubkey.clone(), c.endpoint.clone(), allowed));
    }
    desired.sort();
    desired
}

/// Is the configured endpoint already in effect? `wg` reports the resolved
/// `ip:port`, so a hostname endpoint is only compared by port — and only
/// re-applied when the kernel has no endpoint at all (a NAT'd peer that
/// initiated towards us would otherwise be clobbered every tick).
fn endpoint_satisfied(want: Option<&str>, have: Option<&str>) -> bool {
    match (want, have) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(w), Some(h)) => {
            if w == h {
                return true;
            }
            let literal = w
                .rsplit_once(':')
                .map(|(host, _)| {
                    host.trim_matches(|c| c == '[' || c == ']')
                        .parse::<std::net::IpAddr>()
                        .is_ok()
                })
                .unwrap_or(false);
            if literal {
                false
            } else {
                // hostname: accept whatever it resolved to, as long as the port matches
                w.rsplit_once(':').map(|(_, p)| p) == h.rsplit_once(':').map(|(_, p)| p)
            }
        }
    }
}

/// Delete the WireGuard link if (and only if) it is ours.
pub fn teardown(observed: &Observed, dev: &str) -> Vec<Action> {
    match classify(observed, dev) {
        WgState::Owned => vec![Action::DeleteLink {
            dev: dev.to_owned(),
        }],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{
        kernel::{FakeNetOps, NetOps, fake_pubkey},
        model::{ClientSpec, PeerSpec, WireguardSpec},
    };
    use std::sync::Arc;

    const K1: &str = "EtEvrro2LlkElPgm+ge5ndqtzL2o9ehIWwyIFnKsBQU=";
    const K2: &str = "2Mt9XkPn7iVObR5nCjQYhTe1rtVf6hVuvIG1cyL+zGM=";

    fn spec() -> MeshSpec {
        let mut s = MeshSpec {
            enabled: true,
            box_id: 0,
            underlay: "wg-flock".into(),
            ..Default::default()
        };
        s.wireguard = Some(WireguardSpec::default());
        s.peers = vec![
            PeerSpec {
                pubkey: Some(K1.into()),
                endpoint: Some("10.99.1.1:51820".into()),
                ..PeerSpec::new(1, Ipv4Addr::UNSPECIFIED)
            },
            PeerSpec {
                pubkey: Some(K2.into()),
                ..PeerSpec::new(2, Ipv4Addr::UNSPECIFIED)
            },
        ];
        s.normalize()
    }

    fn keyfile(dir: &Path) -> (std::path::PathBuf, String) {
        let p = dir.join("wg.key");
        std::fs::write(&p, "PRIVTEST\n").unwrap();
        (p, fake_pubkey("PRIVTEST"))
    }

    fn run(ops: &FakeNetOps, s: &MeshSpec, key: &Path, pubkey: &str) -> Vec<Action> {
        let obs = ops.observe(&s.bridge(), Some(&s.underlay)).unwrap();
        let acts = diff(WgInput {
            spec: s,
            observed: &obs,
            key_path: key,
            pubkey,
        });
        for a in &acts {
            ops.apply(a).unwrap();
        }
        acts
    }

    #[test]
    fn create_from_nothing_then_idempotent() {
        let ops = Arc::new(FakeNetOps::new());
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = keyfile(dir.path());
        let s = spec();
        let acts = run(&ops, &s, &key, &pk);
        let kinds: Vec<&str> = acts.iter().map(|a| a.kind()).collect();
        assert_eq!(
            kinds,
            [
                "wg_create",
                "set_alias",
                "wg_set_iface",
                "addr_add",
                "set_mtu",
                "set_up",
                "wg_peer_set",
                "wg_peer_set"
            ]
        );
        // peer 1 has an endpoint, peer 2 doesn't; allowed-ips derived from remote
        let p1 = acts
            .iter()
            .find(|a| matches!(a, Action::WgPeerSet { pubkey, .. } if pubkey == K1))
            .unwrap();
        assert!(
            matches!(p1, Action::WgPeerSet { endpoint: Some(e), allowed_ips, keepalive: 25, .. } if e == "10.99.1.1:51820" && allowed_ips == "10.22.1.1/32")
        );
        let p2 = acts
            .iter()
            .find(|a| matches!(a, Action::WgPeerSet { pubkey, .. } if pubkey == K2))
            .unwrap();
        assert!(
            matches!(p2, Action::WgPeerSet { endpoint: None, allowed_ips, .. } if allowed_ips == "10.22.2.1/32")
        );

        let obs = ops.observe("br-int", Some("wg-flock")).unwrap();
        assert_eq!(classify(&obs, "wg-flock"), WgState::Owned);
        assert_eq!(
            obs.wg.as_ref().unwrap().pubkey.as_deref(),
            Some(pk.as_str())
        );
        assert!(
            run(&ops, &s, &key, &pk).is_empty(),
            "second pass must be a no-op"
        );
    }

    #[test]
    fn peer_changes_are_minimal() {
        let ops = Arc::new(FakeNetOps::new());
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = keyfile(dir.path());
        let mut s = spec();
        run(&ops, &s, &key, &pk);

        // drop peer 2, add peer 3 → one del, one set
        s.peers.remove(1);
        s.peers.push(PeerSpec {
            pubkey: Some("k3".into()),
            ..PeerSpec::new(3, Ipv4Addr::UNSPECIFIED)
        });
        s = s.normalize();
        let acts = run(&ops, &s, &key, &pk);
        let kinds: Vec<&str> = acts.iter().map(|a| a.kind()).collect();
        assert_eq!(kinds, ["wg_peer_set", "wg_peer_del"]);
        assert!(matches!(&acts[1], Action::WgPeerDel { pubkey, .. } if pubkey == K2));

        // keepalive change → every peer re-set
        s.wireguard.as_mut().unwrap().keepalive = 0;
        let acts = run(&ops, &s, &key, &pk);
        assert_eq!(acts.iter().filter(|a| a.kind() == "wg_peer_set").count(), 2);
        assert!(run(&ops, &s, &key, &pk).is_empty());

        // port change → wg_set_iface only
        s.wireguard.as_mut().unwrap().listen_port = 51821;
        let acts = run(&ops, &s, &key, &pk);
        assert_eq!(
            acts.iter().map(|a| a.kind()).collect::<Vec<_>>(),
            ["wg_set_iface"]
        );
    }

    const KC: &str = "4ZJxNbbSoUq4tgYKAa0Ow6EYwlANqldjfPGYdPZ9P2I=";
    const KC2: &str = "aG4yQ0VxS3F0T3J4bW9pN1h0Z2N2VnFhZ1N5Zk1wQkU=";

    fn client(box_id: u8, key: &str) -> ClientSpec {
        ClientSpec {
            box_id,
            pubkey: key.into(),
            name: Some(format!("c{box_id}")),
            endpoint: None,
        }
    }

    #[test]
    fn client_added_is_one_wg_peer_and_nothing_else() {
        let ops = Arc::new(FakeNetOps::new());
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = keyfile(dir.path());
        let mut s = spec();
        run(&ops, &s, &key, &pk);

        s.clients.push(client(3, KC));
        let acts = run(&ops, &s, &key, &pk);
        assert_eq!(acts.len(), 1, "{acts:?}");
        assert!(matches!(
            &acts[0],
            Action::WgPeerSet { pubkey, endpoint: None, allowed_ips, keepalive: 25, .. }
                if pubkey == KC && allowed_ips == "10.22.3.1/32"
        ));
        assert!(run(&ops, &s, &key, &pk).is_empty(), "converged");

        // a client with an endpoint gets it
        s.clients.push(ClientSpec {
            endpoint: Some("198.51.100.7:51820".into()),
            ..client(4, KC2)
        });
        let acts = run(&ops, &s, &key, &pk);
        assert!(matches!(
            &acts[..],
            [Action::WgPeerSet { pubkey, endpoint: Some(e), allowed_ips, .. }]
                if pubkey == KC2 && e == "198.51.100.7:51820" && allowed_ips == "10.22.4.1/32"
        ));
    }

    #[test]
    fn removing_a_client_removes_only_its_wg_peer() {
        let ops = Arc::new(FakeNetOps::new());
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = keyfile(dir.path());
        let mut s = spec();
        s.clients = vec![client(3, KC), client(4, KC2)];
        run(&ops, &s, &key, &pk);

        s.clients.retain(|c| c.box_id != 3);
        let acts = run(&ops, &s, &key, &pk);
        assert_eq!(
            acts,
            vec![Action::WgPeerDel {
                dev: "wg-flock".into(),
                pubkey: KC.into()
            }]
        );
        let obs = ops.observe("br-int", Some("wg-flock")).unwrap();
        let w = obs.wg.unwrap();
        let mut keys: Vec<&str> = w.peers.iter().map(|p| p.pubkey.as_str()).collect();
        keys.sort();
        let mut want = vec![K1, K2, KC2];
        want.sort();
        assert_eq!(keys, want);
    }

    #[test]
    fn client_and_peer_never_share_allowed_ips() {
        // Validation rejects both of these; the planner must still never
        // hand one address to two WireGuard peers.
        let mut s = spec();
        s.clients = vec![
            client(1, KC),  // box 1 is peer K1's: 10.22.1.1/32
            client(5, K2),  // K2 is a peer's key
            client(6, KC2), // fine
        ];
        let d = desired_peers(&s);
        let mut ips: Vec<&str> = d.iter().map(|(_, _, a)| a.as_str()).collect();
        let n = ips.len();
        ips.sort();
        ips.dedup();
        assert_eq!(ips.len(), n, "{d:?}");
        let mut keys: Vec<&str> = d.iter().map(|(k, _, _)| k.as_str()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "{d:?}");
        assert_eq!(n, 3, "peers 1, 2 and client 6: {d:?}");
        let k1 = d.iter().find(|(k, _, _)| k == K1).unwrap();
        assert_eq!(k1.2, "10.22.1.1/32", "the peer keeps its address");
        assert!(d.iter().any(|(k, _, a)| k == KC2 && a == "10.22.6.1/32"));
    }

    #[test]
    fn hostname_endpoint_not_clobbered_once_resolved() {
        assert!(endpoint_satisfied(
            Some("gram.local:51820"),
            Some("10.9.9.9:51820")
        ));
        assert!(!endpoint_satisfied(
            Some("gram.local:51820"),
            Some("10.9.9.9:51821")
        ));
        assert!(!endpoint_satisfied(Some("gram.local:51820"), None));
        assert!(!endpoint_satisfied(
            Some("10.1.1.1:51820"),
            Some("10.9.9.9:51820")
        ));
        assert!(endpoint_satisfied(
            Some("10.1.1.1:51820"),
            Some("10.1.1.1:51820")
        ));
        assert!(endpoint_satisfied(None, Some("1.2.3.4:5")));
    }

    #[test]
    fn foreign_link_is_never_touched() {
        let ops = Arc::new(FakeNetOps::new());
        ops.add_underlay("wg-flock", 1420, Ipv4Addr::new(10, 22, 0, 1)); // kind veth, no alias
        let obs = ops.observe("br-int", Some("wg-flock")).unwrap();
        assert!(matches!(classify(&obs, "wg-flock"), WgState::Foreign(m) if m.contains("veth")));
        assert!(teardown(&obs, "wg-flock").is_empty());
        assert!(obs.wg.is_none());
    }

    #[test]
    fn teardown_only_ours() {
        let ops = Arc::new(FakeNetOps::new());
        let dir = tempfile::tempdir().unwrap();
        let (key, pk) = keyfile(dir.path());
        let s = spec();
        run(&ops, &s, &key, &pk);
        let obs = ops.observe("br-int", Some("wg-flock")).unwrap();
        let t = teardown(&obs, "wg-flock");
        assert_eq!(
            t,
            vec![Action::DeleteLink {
                dev: "wg-flock".into()
            }]
        );
        for a in &t {
            ops.apply(a).unwrap();
        }
        assert!(
            ops.observe("br-int", Some("wg-flock"))
                .unwrap()
                .link("wg-flock")
                .is_none()
        );
    }
}
