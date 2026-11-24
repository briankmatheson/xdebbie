// SPDX-License-Identifier: GPL-3.0-or-later
//! The 0.21 join bug: the package bootstraps every new box as the first box
//! of a cluster of its own (its own CA, certificate and token); with
//! discovery off, `bootstrap --join URL` then found "certificate and token
//! present", said "Already enrolled" and never enrolled. Now the box's own
//! cluster, when empty, is moved aside and the box enrols; one in use is
//! never left behind without --force.

use super::support::{CLUSTER_NETWORK, CaBox, Joiner, TOKEN, approve_when_pending, paths_in};
use crate::bootstrap::{
    api_tls,
    join::{self, JoinOptions},
    takeover::{self, JoinStart, OwnCluster},
};

/// What the package's first-box bootstrap leaves on a new box: its own CA,
/// a server certificate from it, and a token of its own.
fn package_made_own_cluster(j: &Joiner) -> std::path::PathBuf {
    api_tls::ensure_api_tls("joiner", false, &j.paths).unwrap();
    let token = j.dir.path().join("token");
    std::fs::write(&token, "own-cluster-token").unwrap();
    assert!(j.paths.all_exist() && j.paths.is_ca());
    token
}

fn decide_for(
    j: &Joiner,
    token: &std::path::Path,
    url: &str,
    own: OwnCluster,
) -> anyhow::Result<JoinStart> {
    takeover::decide(
        j.paths.all_exist() && token.exists(),
        false,
        j.paths.is_ca(),
        join::load_state(&j.state_dir).as_ref(),
        url,
        move || Ok(own),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_box_with_its_own_empty_cluster_joins() {
    let ca = CaBox::start_with(Some(CLUSTER_NETWORK)).await;
    let j = Joiner::new();
    let token = package_made_own_cluster(&j);
    let own_ca = std::fs::read_to_string(&j.paths.ca_cert).unwrap();

    // before 0.22 this was "Already enrolled; reusing"
    let start = decide_for(&j, &token, &ca.url, OwnCluster::default()).unwrap();
    assert_eq!(start, JoinStart::TakeOverOwn(OwnCluster::default()));
    let moved = takeover::set_aside(&j.paths, &token).unwrap();
    assert_eq!(
        moved.len(),
        5,
        "CA key and cert, server cert and key, token"
    );

    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let (url, fp, paths, state_dir) = (
        ca.url.clone(),
        ca.ca_fingerprint.clone(),
        paths_in(j.dir.path()),
        j.state_dir.clone(),
    );
    let joined = tokio::task::spawn_blocking(move || {
        join::join(&JoinOptions {
            ca_url: &url,
            expect_fingerprint: Some(&fp),
            hostname: "joiner",
            // what bootstrap asks for after a takeover: not the old
            // cluster's id (a first box is 0, the CA box's own)
            box_id: None,
            api_url: "https://joiner:23443",
            force: false,
            paths: &paths,
            state_dir: &state_dir,
        })
    })
    .await
    .unwrap()
    .unwrap();
    approver.await.unwrap();
    assert_eq!(joined.tokens.current, TOKEN);
    assert_eq!(
        joined.box_id, 1,
        "box 0 is the CA box's; the joiner gets the lowest free"
    );
    assert_eq!(
        std::fs::read_to_string(&j.paths.ca_cert).unwrap(),
        ca.ca_pem,
        "now trusts the cluster CA"
    );
    assert!(!j.paths.is_ca(), "a member never holds the CA key");
    // its own cluster's files are kept, not deleted
    assert!(
        moved
            .iter()
            .any(|(_, to)| std::fs::read_to_string(to).unwrap() == own_ca)
    );

    // and a second run is an ordinary re-run: reused, not re-enrolled
    std::fs::write(&token, TOKEN).unwrap();
    assert_eq!(
        decide_for(&j, &token, &ca.url, OwnCluster::default()).unwrap(),
        JoinStart::Reuse
    );
}

#[test]
fn a_box_whose_own_cluster_is_in_use_is_not_taken_over() {
    let j = Joiner::new();
    let token = package_made_own_cluster(&j);
    let busy = OwnCluster {
        vms: 1,
        other_hosts: vec!["box2".into()],
        ..Default::default()
    };
    let e = decide_for(&j, &token, "https://ca-box:23443", busy)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("1 VM") && e.contains("1 other host (box2)") && e.contains("--force"),
        "{e}"
    );
    assert!(j.paths.is_ca(), "nothing moved");
}
