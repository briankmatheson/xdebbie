// SPDX-License-Identifier: GPL-3.0-or-later
//! Box-id assignment at enrolment (BOOTSTRAP.md "Joining", docs/api.md
//! `POST /v1/enroll`): lowest free 0-253, counting the CA box's own id, its
//! peers and clients, and every pending or approved enrolment; a host that
//! re-enrols keeps its id; a denied request holds none.

use std::{collections::BTreeSet, net::Ipv4Addr, sync::Arc};

use xdebbie_core::{EnrollStatus, Enrollment};

use super::support::{CaBox, TOKEN, box_req, queue};
use crate::{
    bootstrap::addr,
    dhcp::model::Cidr,
    enroll::{Decision, Enrollments, MAX_BOX},
    mesh::{MeshSpec, PeerSpec, bridge_mac, underlay_addr},
};

#[tokio::test]
async fn ids_are_unique_and_lowest_free() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let mut seen = BTreeSet::new();
    for i in 0..10 {
        let got = e.submit(box_req(&format!("b{i}"), None).0).await.unwrap();
        assert!(
            seen.insert(got.box_id),
            "box {} handed out twice",
            got.box_id
        );
        if i % 2 == 0 {
            e.decide(&got.id, Decision::Approve).await.unwrap();
        }
    }
    assert_eq!(
        seen,
        (1..=10).collect(),
        "lowest free, the CA box keeping 0"
    );
}

#[tokio::test]
async fn reserved_ids_are_never_assigned_nor_granted_as_a_wish() {
    let dir = tempfile::tempdir().unwrap();
    // the CA box is 5, a config peer is 1, an xdj client is 2
    let e = queue(dir.path(), vec![5, 1, 2]);
    assert_eq!(e.submit(box_req("a", None).0).await.unwrap().box_id, 0);
    assert_eq!(e.submit(box_req("b", None).0).await.unwrap().box_id, 3);
    for taken in [5, 1, 2, 0, 3] {
        let err = e
            .submit(box_req("c", Some(taken)).0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("already in use"), "box {taken}: {err}");
    }
    assert_eq!(e.submit(box_req("c", Some(9)).0).await.unwrap().box_id, 9);
    // a wish beyond the last box is refused, not wrapped
    let err = e
        .submit(box_req("d", Some(MAX_BOX + 1)).0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("out of range"), "{err}");
}

#[tokio::test]
async fn an_approved_box_keeps_its_id_across_restart_and_re_enrolment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enroll.json");
    let a_id;
    {
        let e = queue(dir.path(), vec![0]);
        let a = e.submit(box_req("a", None).0).await.unwrap();
        let b = e.submit(box_req("b", None).0).await.unwrap();
        e.decide("a", Decision::Approve).await.unwrap();
        (a_id, _) = (a.box_id, b.box_id);
        assert_eq!((a.box_id, b.box_id), (1, 2));
    }
    // Daemon restart: same file, same CA key on disk.
    let e = queue(dir.path(), vec![0]);
    // the approved and the still-pending one both hold their ids
    assert_eq!(e.submit(box_req("c", None).0).await.unwrap().box_id, 3);
    // a re-enrolment (bootstrap --join --force: new key) keeps the old id,
    // even when it asks for another one
    let again = e.submit(box_req("a", Some(40)).0).await.unwrap();
    assert_eq!(again.box_id, a_id);
    assert_eq!(again.status, EnrollStatus::Pending, "a new key waits");
    let ok = e.decide("a", Decision::Approve).await.unwrap();
    assert_eq!((ok.box_id, ok.status), (a_id, EnrollStatus::Approved));
    // and once more after a second restart
    drop(e);
    let e = Enrollments::load(path, None, vec![0]).unwrap();
    let all = e.list(true).await;
    assert!(
        all.iter()
            .filter(|x| x.hostname == "a")
            .all(|x| x.box_id == a_id)
    );
}

#[tokio::test]
async fn same_key_again_is_the_same_enrolment_and_id() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let (r, _) = box_req("a", None);
    let first = e.submit(r.clone()).await.unwrap();
    e.decide("a", Decision::Approve).await.unwrap();
    let again = e.submit(r).await.unwrap();
    assert_eq!((again.id, again.box_id), (first.id, first.box_id));
    assert_eq!(again.status, EnrollStatus::Approved);
    assert_eq!(e.list(true).await.len(), 1);
}

#[tokio::test]
async fn a_present_box_id_is_never_handed_to_another_host() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let a = e.submit(box_req("a", None).0).await.unwrap();
    e.decide("a", Decision::Approve).await.unwrap();
    // a pending re-key of "a" does not free 1 for anyone else either
    e.submit(box_req("a", None).0).await.unwrap();
    for i in 0..5 {
        let x = e.submit(box_req(&format!("x{i}"), None).0).await.unwrap();
        assert_ne!(x.box_id, a.box_id);
    }
    let err = e
        .submit(box_req("y", Some(a.box_id)).0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already in use"), "{err}");
}

/// The documented reuse policy: a denied request holds no id (the next
/// joiner may get it); an approved enrolment holds its id for good — there
/// is no "forget this box" in the queue, so a removed box's id is not reused.
#[tokio::test]
async fn denied_requests_free_their_id_approved_ones_never_do() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let a = e.submit(box_req("a", None).0).await.unwrap();
    let b = e.submit(box_req("b", None).0).await.unwrap();
    e.decide("a", Decision::Approve).await.unwrap();
    e.decide(&b.id, Decision::Deny).await.unwrap();
    let c = e.submit(box_req("c", None).0).await.unwrap();
    assert_eq!(c.box_id, b.box_id, "the denied request's id is free again");
    let d = e.submit(box_req("d", None).0).await.unwrap();
    assert_ne!(d.box_id, a.box_id);
}

/// `xdctl enroll deny` on a box that is already approved must not free its
/// id: the box is still out there with a valid certificate (there is no
/// revocation) and still uses `10.22.<box>.1` and its bridge MAC.
#[tokio::test]
async fn denying_an_approved_box_does_not_free_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), vec![0]);
    let a = e.submit(box_req("a", None).0).await.unwrap();
    e.decide("a", Decision::Approve).await.unwrap();
    let err = e.decide("a", Decision::Deny).await.unwrap_err().to_string();
    assert!(err.contains("already approved"), "{err}");
    let still = e.get(&a.id).await.unwrap();
    assert_eq!(still.status, EnrollStatus::Approved, "deny is refused");
    assert!(still.cert.is_some());
    let next = e.submit(box_req("b", None).0).await.unwrap();
    assert_ne!(next.box_id, a.box_id, "box {} handed out twice", a.box_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_requests_and_approvals_never_collide() {
    let dir = tempfile::tempdir().unwrap();
    let e = Arc::new(queue(dir.path(), vec![0]));
    // CSRs first (slow-ish), then everything at once
    let reqs: Vec<_> = (0..40).map(|i| box_req(&format!("c{i}"), None).0).collect();
    let mut tasks = vec![];
    for r in reqs {
        let e = e.clone();
        tasks.push(tokio::spawn(async move {
            let got = e.submit(r).await.unwrap();
            e.decide(&got.id, Decision::Approve).await.unwrap()
        }));
    }
    let mut ids = BTreeSet::new();
    for t in tasks {
        let got: Enrollment = t.await.unwrap();
        assert_eq!(got.status, EnrollStatus::Approved);
        assert!(
            ids.insert(got.box_id),
            "box {} handed out twice",
            got.box_id
        );
    }
    assert_eq!(ids, (1..=40).collect());
    // what is on disk agrees
    let reloaded = Enrollments::load(dir.path().join("enroll.json"), None, vec![]).unwrap();
    let on_disk: BTreeSet<u8> = reloaded.list(true).await.iter().map(|x| x.box_id).collect();
    assert_eq!(on_disk, ids);
}

#[tokio::test]
async fn running_out_of_ids_is_an_error_not_a_collision() {
    let dir = tempfile::tempdir().unwrap();
    let e = queue(dir.path(), (0..=250).collect());
    let mut got = vec![];
    for h in ["p", "q", "r"] {
        got.push(e.submit(box_req(h, None).0).await.unwrap().box_id);
    }
    assert_eq!(got, [251, 252, MAX_BOX]);
    let err = e
        .submit(box_req("s", None).0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no free box id"), "{err}");
    // nothing was queued for the refused one
    assert!(!e.list(true).await.iter().any(|x| x.hostname == "s"));
    // a denial makes room again
    e.decide("q", Decision::Deny).await.unwrap();
    assert_eq!(e.submit(box_req("s", None).0).await.unwrap().box_id, 252);
}

/// Every id the queue can hand out has its own /24 of the overlay (bridge
/// .1, VMs .2-.254), its own WireGuard underlay address and bridge MAC, and
/// the mesh's derived probe lands on that bridge.
#[test]
fn every_assignable_id_maps_to_its_own_slash_24_underlay_and_mac() {
    let cidr: Cidr = "10.23.0.0/16".parse().unwrap();
    assert!(
        addr::capacity(&cidr) > u32::from(MAX_BOX),
        "the default overlay must have room for every box id"
    );
    let spec = MeshSpec::default();
    let mut bridges = BTreeSet::new();
    let mut underlays = BTreeSet::new();
    let mut macs = BTreeSet::new();
    let mut last_end = None::<u32>;
    for b in 0..=MAX_BOX {
        let blk = addr::box_block(&cidr, b).unwrap();
        assert_eq!(blk.bridge, Ipv4Addr::new(10, 23, b, 1));
        assert_eq!(blk.pool_start, Ipv4Addr::new(10, 23, b, 2));
        assert_eq!(blk.pool_end, Ipv4Addr::new(10, 23, b, 254));
        assert_eq!(blk.network(), Ipv4Addr::new(10, 23, b, 0));
        if let Some(prev) = last_end {
            assert!(u32::from(blk.pool_start) > prev, "pools overlap at box {b}");
        }
        last_end = Some(u32::from(blk.pool_end));
        let p = PeerSpec::new(b, underlay_addr(b));
        assert_eq!(spec.probe_addr(&p), blk.bridge, "probe for box {b}");
        assert_eq!(underlay_addr(b), Ipv4Addr::new(10, 22, b, 1));
        assert_eq!(bridge_mac(b), format!("52:54:00:23:{b:02x}:fe"));
        bridges.insert(blk.bridge);
        underlays.insert(underlay_addr(b));
        macs.insert(bridge_mac(b));
    }
    let n = usize::from(MAX_BOX) + 1;
    assert_eq!((bridges.len(), underlays.len(), macs.len()), (n, n, n));
    // the mesh accepts exactly the ids enrolment hands out
    let mut s = MeshSpec {
        box_id: MAX_BOX,
        ..Default::default()
    };
    let env = crate::mesh::validate::Env::default();
    let box_errs = |s: &MeshSpec| {
        crate::mesh::validate::validate(s, &env)
            .into_iter()
            .filter(|e| e.field == "box")
            .count()
    };
    assert_eq!(box_errs(&s), 0);
    s.box_id = MAX_BOX + 1;
    assert_eq!(box_errs(&s), 1);
}

/// Peers and xdj clients added through the API (`xdctl mesh peer add`,
/// `xdctl mesh client add`) use their box numbers just as much as the ones
/// in the CA box's TOML: enrolment must not hand them out again.
#[tokio::test]
async fn ids_in_the_live_mesh_spec_are_not_handed_out() {
    let ca = CaBox::start().await;
    let spec = MeshSpec {
        enabled: true,
        underlay: "ul0".into(),
        allow_plain_underlay: true,
        peers: vec![
            PeerSpec::new(1, Ipv4Addr::new(10, 22, 1, 1)),
            PeerSpec::new(2, Ipv4Addr::new(10, 22, 2, 1)),
        ],
        ..Default::default()
    };
    assert!(ca.mesh.put(spec, None).await.is_ok(), "mesh spec accepted");
    let (r, _) = box_req("joiner", None);
    let got: Enrollment = ca
        .anon()
        .post(format!("{}/v1/enroll", ca.url))
        .json(&r)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got.box_id, 3, "boxes 1 and 2 are mesh peers already");
    // and a wish for one of them is refused
    let (mut r, _) = box_req("other", None);
    r.box_id = Some(2);
    let resp = ca
        .anon()
        .post(format!("{}/v1/enroll", ca.url))
        .bearer_auth(TOKEN)
        .json(&r)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}
