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
            if n.bridge.is_empty() || n.bridge.len() > 15 || n.bridge.contains(char::is_whitespace) {
                err(&mut e, "networks[0].bridge", "must be an interface name (1–15 chars, no spaces)");
            }
            if n.vni == 0 || n.vni > 0x00FF_FFFF {
                err(&mut e, "networks[0].vni", format!("must be 1–16777215, got {}", n.vni));
            }
        }
        0 => err(&mut e, "networks", "v1 requires exactly one {bridge, vni} entry; got none"),
        n => err(&mut e, "networks", format!("v1 accepts exactly one {{bridge, vni}} entry; got {n}")),
    }

    if spec.dstport == 0 {
        err(&mut e, "dstport", "must be 1–65535");
    }

    if spec.underlay.is_empty() || spec.underlay.len() > 15 || spec.underlay.contains(char::is_whitespace) {
        err(&mut e, "underlay", "must be an interface name (1–15 chars, no spaces)");
    }

    if let MtuSpec::Fixed(m) = spec.mtu {
        if m < MIN_MTU {
            err(&mut e, "mtu", format!("must be at least {MIN_MTU}, got {m}"));
        } else if let Some(u) = env.underlay_mtu {
            let max = auto_mtu(u);
            if m > max {
                err(&mut e, "mtu", format!("{m} exceeds the computed maximum {max} (underlay mtu {u} − {GENEVE_OVERHEAD})"));
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
        err(&mut e, "probe.timeout_ms", "must not exceed probe.interval_ms");
    }
    if spec.probe.down_after == 0 {
        err(&mut e, "probe.down_after", "must be ≥ 1");
    }
    if spec.probe.up_after == 0 {
        err(&mut e, "probe.up_after", "must be ≥ 1");
    }

    let local: HashSet<Ipv4Addr> = env.local_addrs.iter().copied().collect();
    let mut boxes = HashSet::new();
    let mut remotes = HashSet::new();
    for (i, p) in spec.peers.iter().enumerate() {
        let f = |name: &str| format!("peers[{i}].{name}");
        if p.box_id > 253 {
            err(&mut e, f("box"), format!("must be 0–253, got {}", p.box_id));
        }
        if p.box_id == spec.box_id {
            err(&mut e, f("box"), format!("{} is the local box number", p.box_id));
        }
        if !boxes.insert(p.box_id) {
            err(&mut e, f("box"), format!("duplicate peer box {}", p.box_id));
        }
        if p.remote.is_unspecified() || p.remote.is_loopback() || p.remote.is_broadcast() || p.remote.is_multicast() {
            err(&mut e, f("remote"), format!("{} is not a usable unicast address", p.remote));
        }
        if local.contains(&p.remote) {
            err(&mut e, f("remote"), format!("{} is one of this host's own addresses", p.remote));
        }
        if !remotes.insert(p.remote) {
            err(&mut e, f("remote"), format!("duplicate remote {}", p.remote));
        }
        if let Some(pr) = p.probe
            && (pr.is_unspecified() || pr.is_broadcast() || pr.is_multicast())
        {
            err(&mut e, f("probe"), format!("{pr} is not a usable unicast address"));
        }
    }
    e
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
                PeerSpec { box_id: 1, remote: "10.22.1.1".parse().unwrap(), probe: None },
                PeerSpec { box_id: 2, remote: "10.22.2.1".parse().unwrap(), probe: None },
            ],
            ..Default::default()
        }
    }
    fn env() -> Env {
        Env { local_addrs: vec!["10.22.0.1".parse().unwrap()], underlay_mtu: Some(1420) }
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
        s.networks.push(NetworkSpec { bridge: "br2".into(), vni: 24 });
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
}
