// SPDX-License-Identifier: GPL-3.0-or-later
//! Joining a box that was set up before: the regressions found bringing
//! simplyaviary (box 5) into the timesink cluster on 2026-11-25. It had
//! first been bootstrapped as a box of its own (box 0), then enrolled.
//! The mesh side of the same day (a stale underlay address, a bridge
//! holding the hub's address) is covered next to the other mesh tests in
//! `mesh::tests`; libvirtd's `listen_addr` in `bootstrap::tls_certs`.

use super::support::*;
use crate::bootstrap::join::JoinState;
use crate::bootstrap::join::normalize_join_url;
use crate::bootstrap::takeover::{JoinStart, OwnCluster, decide};
use crate::enroll::Decision;
use std::net::{IpAddr, Ipv4Addr};

fn ip_sans(cert_pem: &str) -> Vec<IpAddr> {
    let (_, c) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
    let (_, x) = x509_parser::parse_x509_certificate(&c.contents).unwrap();
    x.subject_alternative_name()
        .ok()
        .flatten()
        .map(|s| {
            s.value
                .general_names
                .iter()
                .filter_map(|g| match g {
                    x509_parser::extensions::GeneralName::IPAddress(b) if b.len() == 4 => {
                        Some(IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3])))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A joiner asks for its certificate at step 4 and creates its bridge at
/// step 5, so its request never names the bridge address libvirtd listens
/// on. The CA knows the box id it assigns: the certificate must carry that
/// box's underlay and bridge addresses anyway (simplyaviary needed a second,
/// `--force`, enrolment for 10.23.5.1).
#[tokio::test]
async fn approval_puts_the_assigned_boxs_mesh_addresses_in_the_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let (r, _csr) = box_req("joiner.example", Some(5));
    let pending = e.submit(r).await.unwrap();
    assert_eq!(pending.box_id, 5);
    let ok = e.decide("joiner.example", Decision::Approve).await.unwrap();
    let sans = ip_sans(ok.cert.as_deref().unwrap());
    for want in [Ipv4Addr::new(10, 22, 5, 1), Ipv4Addr::new(10, 23, 5, 1)] {
        assert!(
            sans.contains(&IpAddr::V4(want)),
            "{want} missing from {sans:?}"
        );
    }
    // and not some other box's
    assert!(
        !sans.contains(&IpAddr::V4(Ipv4Addr::new(10, 23, 0, 1))),
        "{sans:?}"
    );
}

/// `--join timesink` on a box enrolled as `https://timesink:23443` was
/// refused as another cluster. Every spelling of one URL is one cluster.
#[test]
fn every_spelling_of_the_ca_url_is_the_same_cluster() {
    for typed in [
        "timesink",
        "timesink:23443",
        "https://timesink:23443",
        "https://timesink:23443/",
        "  timesink ",
        "HTTPS://timesink",
    ] {
        assert_eq!(
            normalize_join_url(typed),
            "https://timesink:23443",
            "{typed:?}"
        );
    }
    assert_eq!(
        normalize_join_url("192.168.1.157"),
        "https://192.168.1.157:23443"
    );
    assert_eq!(normalize_join_url("node0:8443"), "https://node0:8443");
    assert_eq!(normalize_join_url("[fd00::1]"), "https://[fd00::1]:23443");
    assert_eq!(
        normalize_join_url("[fd00::1]:9000"),
        "https://[fd00::1]:9000"
    );

    let prior = JoinState {
        ca_url: "https://timesink:23443".into(),
        ca_fingerprint: "sha256:aa".into(),
        enrollment_id: "e1".into(),
        key_fingerprint: "sha256:bb".into(),
    };
    for typed in ["timesink", "https://timesink:23443/"] {
        let d = decide(
            true,
            false,
            false,
            Some(&prior),
            &normalize_join_url(typed),
            || Ok(OwnCluster::default()),
        )
        .unwrap();
        assert_eq!(d, JoinStart::Reuse, "{typed:?}");
    }
    // a join.json written by an older xdebbied, holding the bare name
    let old = JoinState {
        ca_url: "timesink".into(),
        ..prior.clone()
    };
    let d = decide(
        true,
        false,
        false,
        Some(&old),
        "https://timesink:23443",
        || Ok(OwnCluster::default()),
    )
    .unwrap();
    assert_eq!(d, JoinStart::Reuse);
    // a really different cluster is still refused without --force
    assert!(
        decide(
            true,
            false,
            false,
            Some(&prior),
            "https://node0:23443",
            || { Ok(OwnCluster::default()) }
        )
        .is_err()
    );
}
