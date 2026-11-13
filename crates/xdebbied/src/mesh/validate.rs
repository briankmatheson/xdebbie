// SPDX-License-Identifier: GPL-3.0-or-later
//! Section 3.2 rules. Pure: everything the rules need from the kernel is
//! passed in as `Env`, so the API and the config loader share one checker
//! and the tests need no root.

use std::{collections::HashSet, net::Ipv4Addr};

use serde::Serialize;

use super::model::*;

/// What validation needs to know about the host.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// IPv4 addresses configured on this host (a peer `remote` can't be one).
    pub local_addrs: Vec<Ipv4Addr>,
    /// MTU of the underlay interface, if it exists (bounds a fixed `mtu`).
    pub underlay_mtu: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

fn err(errs: &mut Vec<FieldError>, field: impl Into<String>, message: impl Into<String>) {
    errs.push(FieldError {
        field: field.into(),
        message: message.into(),
    });
}

/// Every rule in section 3.2. Empty result = valid.
pub fn validate(spec: &MeshSpec, env: &Env) -> Vec<FieldError> {
    let mut e = Vec::new();

    if spec.box_id > 253 {
        err(&mut e, "box", format!("must be 0–253, got {}", spec.box_id));
    }

    match spec.networks.len() {
        1 => {
            let n = &spec.networks[0];
            if n.bridge.is_empty() || n.bridge.len() > 15 || n.bridge.contains(char::is_whitespace)
            {
                err(
                    &mut e,
                    "networks[0].bridge",
                    "must be an interface name (1–15 chars, no spaces)",
                );
            }
            if n.vni == 0 || n.vni > 0x00FF_FFFF {
                err(
                    &mut e,
                    "networks[0].vni",
                    format!("must be 1–16777215, got {}", n.vni),
                );
            }
        }
        0 => err(
            &mut e,
            "networks",
            "v1 requires exactly one {bridge, vni} entry; got none",
        ),
        n => err(
            &mut e,
            "networks",
            format!("v1 accepts exactly one {{bridge, vni}} entry; got {n}"),
        ),
    }

    if spec.dstport == 0 {
        err(&mut e, "dstport", "must be 1–65535");
    }

    if spec.underlay.is_empty()
        || spec.underlay.len() > 15
        || spec.underlay.contains(char::is_whitespace)
    {
        err(
            &mut e,
            "underlay",
            "must be an interface name (1–15 chars, no spaces)",
        );
    }

    if let MtuSpec::Fixed(m) = spec.mtu {
        if m < MIN_MTU {
            err(
                &mut e,
                "mtu",
                format!("must be at least {MIN_MTU}, got {m}"),
            );
        } else if let Some(u) = env.underlay_mtu {
            let max = auto_mtu(u);
            if m > max {
                err(
                    &mut e,
                    "mtu",
                    format!(
                        "{m} exceeds the computed maximum {max} (underlay mtu {u} − {GENEVE_OVERHEAD})"
                    ),
                );
            }
        }
    }

    if spec.probe.interval_ms == 0 {
        err(&mut e, "probe.interval_ms", "must be > 0");
    }
    if spec.probe.timeout_ms == 0 {
        err(&mut e, "probe.timeout_ms", "must be > 0");
    }
    if spec.probe.timeout_ms > spec.probe.interval_ms {
        err(
            &mut e,
            "probe.timeout_ms",
            "must not exceed probe.interval_ms",
        );
    }
    if spec.probe.down_after == 0 {
        err(&mut e, "probe.down_after", "must be ≥ 1");
    }
    if spec.probe.up_after == 0 {
        err(&mut e, "probe.up_after", "must be ≥ 1");
    }

    if let Some(w) = &spec.wireguard {
        if w.listen_port == 0 {
            err(&mut e, "wireguard.listen_port", "must be 1–65535");
        }
        if !(8..=30).contains(&w.prefix) {
            err(
                &mut e,
                "wireguard.prefix",
                format!("must be 8–30, got {}", w.prefix),
            );
        }
        if w.mtu < MIN_MTU + GENEVE_OVERHEAD {
            err(
                &mut e,
                "wireguard.mtu",
                format!(
                    "must be at least {} so the overlay keeps {MIN_MTU}",
                    MIN_MTU + GENEVE_OVERHEAD
                ),
            );
        }
        if let Some(a) = w.address
            && (a.is_unspecified() || a.is_loopback() || a.is_broadcast() || a.is_multicast())
        {
            err(
                &mut e,
                "wireguard.address",
                format!("{a} is not a usable unicast address"),
            );
        }
    }

    let local: HashSet<Ipv4Addr> = env.local_addrs.iter().copied().collect();
    let mut boxes = HashSet::new();
    let mut remotes = HashSet::new();
    let mut pubkeys = HashSet::new();
    let wg_addr = spec.wg_address();
    for (i, p) in spec.peers.iter().enumerate() {
        let f = |name: &str| format!("peers[{i}].{name}");
        if p.box_id > 253 {
            err(&mut e, f("box"), format!("must be 0–253, got {}", p.box_id));
        }
        if p.box_id == spec.box_id {
            err(
                &mut e,
                f("box"),
                format!("{} is the local box number", p.box_id),
            );
        }
        if !boxes.insert(p.box_id) {
            err(&mut e, f("box"), format!("duplicate peer box {}", p.box_id));
        }
        if p.remote.is_unspecified() {
            err(
                &mut e,
                f("remote"),
                "required (derived automatically only when `wireguard` is set)",
            );
        } else if p.remote.is_loopback() || p.remote.is_broadcast() || p.remote.is_multicast() {
            err(
                &mut e,
                f("remote"),
                format!("{} is not a usable unicast address", p.remote),
            );
        }
        if wg_addr == Some(p.remote) {
            err(
                &mut e,
                f("remote"),
                format!("{} is this box's own WireGuard address", p.remote),
            );
        }
        if local.contains(&p.remote) {
            err(
                &mut e,
                f("remote"),
                format!("{} is one of this host's own addresses", p.remote),
            );
        }
        if !remotes.insert(p.remote) {
            err(
                &mut e,
                f("remote"),
                format!("duplicate remote {}", p.remote),
            );
        }
        if let Some(pr) = p.probe
            && (pr.is_unspecified() || pr.is_broadcast() || pr.is_multicast())
        {
            err(
                &mut e,
                f("probe"),
                format!("{pr} is not a usable unicast address"),
            );
        }
        match (&spec.wireguard, &p.pubkey) {
            (Some(_), None) => err(&mut e, f("pubkey"), "required when `wireguard` is set"),
            (None, Some(_)) => err(
                &mut e,
                f("pubkey"),
                "only meaningful when `wireguard` is set",
            ),
            (Some(_), Some(k)) => {
                if !valid_wg_key(k) {
                    err(
                        &mut e,
                        f("pubkey"),
                        "must be a 32-byte base64 key (44 chars ending in '=')",
                    );
                }
                if !pubkeys.insert(k.clone()) {
                    err(&mut e, f("pubkey"), "duplicate public key");
                }
            }
            (None, None) => {}
        }
        if let Some(ep) = &p.endpoint {
            if spec.wireguard.is_none() {
                err(
                    &mut e,
                    f("endpoint"),
                    "only meaningful when `wireguard` is set",
                );
            } else if !valid_endpoint(ep) {
                err(
                    &mut e,
                    f("endpoint"),
                    "must be host:port or [v6]:port with a non-zero port",
                );
            }
        }
    }
    e
}

/// 44-char base64 of 32 bytes: `[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=`.
pub fn valid_wg_key(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() == 44
        && b[..43]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
        && b[43] == b'='
        && b"AEIMQUYcgkosw048".contains(&b[42])
}

/// `host:port` / `[v6]:port`; the host part is left to `wg` to resolve.
pub fn valid_endpoint(ep: &str) -> bool {
    let Some((host, port)) = ep.rsplit_once(':') else {
        return false;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    !host.is_empty()
        && !host.contains(char::is_whitespace)
        && port.parse::<u16>().map(|p| p != 0).unwrap_or(false)
}

/// Convenience for the config loader: one error naming every bad field.
pub fn validate_or_err(spec: &MeshSpec, env: &Env) -> anyhow::Result<()> {
    let errs = validate(spec, env);
    if errs.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
    anyhow::bail!("invalid [mesh] configuration: {}", list.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> MeshSpec {
        MeshSpec {
            enabled: true,
            box_id: 0,
            underlay: "ul0".into(),
            peers: vec![
                PeerSpec::new(1, "10.22.1.1".parse().unwrap()),
                PeerSpec::new(2, "10.22.2.1".parse().unwrap()),
            ],
            ..Default::default()
        }
    }
    fn env() -> Env {
        Env {
            local_addrs: vec!["10.22.0.1".parse().unwrap()],
            underlay_mtu: Some(1420),
        }
    }
    fn fields(v: &[FieldError]) -> Vec<&str> {
        v.iter().map(|e| e.field.as_str()).collect()
    }

    #[test]
    fn valid_spec_has_no_errors() {
        assert!(validate(&base(), &env()).is_empty());
    }

    #[test]
    fn box_range() {
        let mut s = base();
        s.box_id = 254;
        assert_eq!(fields(&validate(&s, &env())), vec!["box"]);
    }

    #[test]
    fn peer_cannot_be_local_box() {
        let mut s = base();
        s.peers[0].box_id = 0;
        assert_eq!(fields(&validate(&s, &env())), vec!["peers[0].box"]);
    }

    #[test]
    fn peer_boxes_unique() {
        let mut s = base();
        s.peers[1].box_id = 1;
        let v = validate(&s, &env());
        assert_eq!(fields(&v), vec!["peers[1].box"]);
        assert!(v[0].message.contains("duplicate"));
    }

    #[test]
    fn remotes_unique_and_not_local() {
        let mut s = base();
        s.peers[1].remote = s.peers[0].remote;
        assert_eq!(fields(&validate(&s, &env())), vec!["peers[1].remote"]);
        let mut s = base();
        s.peers[0].remote = "10.22.0.1".parse().unwrap();
        let v = validate(&s, &env());
        assert_eq!(fields(&v), vec!["peers[0].remote"]);
        assert!(v[0].message.contains("own addresses"));
    }

    #[test]
    fn networks_exactly_one() {
        let mut s = base();
        s.networks.clear();
        assert_eq!(fields(&validate(&s, &env())), vec!["networks"]);
        let mut s = base();
        s.networks.push(NetworkSpec {
            bridge: "br2".into(),
            vni: 24,
        });
        assert_eq!(fields(&validate(&s, &env())), vec!["networks"]);
    }

    #[test]
    fn vni_and_port_ranges() {
        let mut s = base();
        s.networks[0].vni = 0;
        assert_eq!(fields(&validate(&s, &env())), vec!["networks[0].vni"]);
        s.networks[0].vni = 1 << 24;
        assert_eq!(fields(&validate(&s, &env())), vec!["networks[0].vni"]);
        let mut s = base();
        s.dstport = 0;
        assert_eq!(fields(&validate(&s, &env())), vec!["dstport"]);
    }

    #[test]
    fn mtu_bounds() {
        let mut s = base();
        s.mtu = MtuSpec::Fixed(1279);
        assert_eq!(fields(&validate(&s, &env())), vec!["mtu"]);
        s.mtu = MtuSpec::Fixed(1371);
        let v = validate(&s, &env());
        assert_eq!(fields(&v), vec!["mtu"]);
        assert!(v[0].message.contains("1370"));
        s.mtu = MtuSpec::Fixed(1370);
        assert!(validate(&s, &env()).is_empty());
        // Unknown underlay MTU: only the lower bound is checked.
        s.mtu = MtuSpec::Fixed(9000);
        assert!(validate(&s, &Env::default()).is_empty());
    }

    #[test]
    fn probe_params() {
        let mut s = base();
        s.probe.timeout_ms = 2000;
        assert_eq!(fields(&validate(&s, &env())), vec!["probe.timeout_ms"]);
        let mut s = base();
        s.probe.down_after = 0;
        assert_eq!(fields(&validate(&s, &env())), vec!["probe.down_after"]);
    }

    #[test]
    fn underlay_name() {
        let mut s = base();
        s.underlay = "".into();
        assert_eq!(fields(&validate(&s, &env())), vec!["underlay"]);
    }

    #[test]
    fn config_loader_error_names_fields() {
        let mut s = base();
        s.peers[0].box_id = 0;
        let e = validate_or_err(&s, &env()).unwrap_err().to_string();
        assert!(e.contains("peers[0].box"), "{e}");
    }

    #[test]
    fn wireguard_rules() {
        let mut s = base();
        s.wireguard = Some(WireguardSpec::default());
        s.peers = vec![PeerSpec::new(1, Ipv4Addr::new(10, 22, 1, 1))];
        let e = validate(&s, &Env::default());
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].field, "peers[0].pubkey");

        s.peers[0].pubkey = Some("EtEvrro2LlkElPgm+ge5ndqtzL2o9ehIWwyIFnKsBQU=".into());
        s.peers[0].endpoint = Some("gram.local:51820".into());
        assert!(validate(&s, &Env::default()).is_empty());

        s.peers.push(PeerSpec {
            pubkey: s.peers[0].pubkey.clone(),
            ..PeerSpec::new(2, Ipv4Addr::new(10, 22, 2, 1))
        });
        s.peers[1].endpoint = Some("nohost".into());
        let e = validate(&s, &Env::default());
        let fields: Vec<&str> = e.iter().map(|x| x.field.as_str()).collect();
        assert!(
            fields.contains(&"peers[1].pubkey") && fields.contains(&"peers[1].endpoint"),
            "{e:?}"
        );

        // own address as a remote, bad key shape, bad port/prefix
        s.peers.truncate(1);
        s.peers[0].remote = Ipv4Addr::new(10, 22, 0, 1);
        s.peers[0].pubkey = Some("short".into());
        s.wireguard.as_mut().unwrap().listen_port = 0;
        s.wireguard.as_mut().unwrap().prefix = 31;
        let e = validate(&s, &Env::default());
        let fields: Vec<&str> = e.iter().map(|x| x.field.as_str()).collect();
        for f in [
            "peers[0].remote",
            "peers[0].pubkey",
            "wireguard.listen_port",
            "wireguard.prefix",
        ] {
            assert!(fields.contains(&f), "missing {f} in {e:?}");
        }

        // pubkey without wireguard; missing remote without wireguard
        let mut s = base();
        s.peers = vec![PeerSpec {
            pubkey: Some("x".into()),
            ..PeerSpec::new(1, Ipv4Addr::UNSPECIFIED)
        }];
        let e = validate(&s, &Env::default());
        let fields: Vec<&str> = e.iter().map(|x| x.field.as_str()).collect();
        assert!(
            fields.contains(&"peers[0].remote") && fields.contains(&"peers[0].pubkey"),
            "{e:?}"
        );

        assert!(
            valid_endpoint("[fe80::1]:51820")
                && !valid_endpoint("host:0")
                && !valid_endpoint(":51820")
        );
    }
}
