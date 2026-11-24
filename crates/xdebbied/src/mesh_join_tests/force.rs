// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdebbied bootstrap --force`: a new config from the template, the
//! operator's settings carried over, and the box id kept — on a joiner the
//! CA box hands the same id back to the re-keyed host.

use std::net::Ipv4Addr;

use super::support::{CLUSTER_NETWORK, CaBox, Joiner, approve_when_pending, cluster_status};
use crate::bootstrap::{
    config_gen::{generate_config_for, regenerate},
    join::{self, JoinOptions},
    libvirt_setup::{LibvirtResources, NetworkInfo},
};

/// A joiner's config after months of use: hand-tuned and hand-edited.
const LIVED_IN: &str = r#"
listen = "0.0.0.0:23443"
state_file = "/var/lib/xdebbie/state.json"
ca_url = "https://old-ca:23443"
advertise_url = "https://joiner.lan:23443"
policy = "pack"
volume_group = "joiner-vg"
thin_pool = "abe"
reconcile_interval_secs = 17
host_stale_secs = 99

[network]
cidr = "10.23.0.0/16"
gateway = "10.23.7.1"
dns = ["10.23.7.1"]
pool_start = "10.23.7.20"
pool_end = "10.23.7.200"
ntp = ["10.23.0.1"]
lease_secs = 600

[mesh]
enabled = true
box = 7
underlay = "wg-flock"
allow_plain_underlay = true

[mesh.wireguard]
listen_port = 51820

[[mesh.peers]]
box = 0
pubkey = "k0"
endpoint = "ca-box.lan:51820"

[[mesh.peers]]
box = 1
pubkey = "k1"
endpoint = "gram.lan:51820"

[[hosts]]
name = "joiner.lan"
uri = "qemu:///system"
cpu_overcommit = 3.0
"#;

fn template(box_id: u8, join: Option<&str>) -> String {
    let resources = LibvirtResources {
        networks: vec![NetworkInfo {
            name: "xdebbie".into(),
            bridge: "virbr-xdebbie".into(),
        }],
        pools: vec![],
    };
    let cluster = cluster_status(CLUSTER_NETWORK);
    generate_config_for(
        &resources,
        "qemu:///system",
        None,
        box_id,
        join,
        "xdebbie",
        "xdebbie",
        Some(&cluster),
        "joiner.example",
        &[],
    )
}

fn load(c: &str) -> crate::config::Config {
    toml::from_str(c).unwrap_or_else(|e| panic!("{e}\n{c}"))
}

#[test]
fn force_keeps_the_box_id_and_every_operator_setting() {
    let generated = template(7, Some("https://ca-box:23443"));
    let out = regenerate(LIVED_IN, &generated, true, true, 7).unwrap();
    let cfg = load(&out);
    let mesh = cfg.mesh.as_ref().unwrap();
    assert_eq!(mesh.box_id, 7);
    // peers keep their own numbers (only [mesh] box is rewritten)
    let peer_boxes: Vec<u8> = mesh.peers.iter().map(|p| p.box_id).collect();
    assert_eq!(peer_boxes, [0, 1]);
    assert!(mesh.enabled && mesh.wireguard.is_some());
    assert!(out.contains("policy = \"pack\"") && !out.contains("policy = \"spread\""));
    assert_eq!(cfg.volume_group.as_deref(), Some("joiner-vg"));
    assert_eq!(cfg.thin_pool.as_deref(), Some("abe"));
    assert_eq!(
        cfg.advertise_url.as_deref(),
        Some("https://joiner.lan:23443")
    );
    assert!(out.contains("reconcile_interval_secs = 17"), "{out}");
    assert!(out.contains("host_stale_secs = 99"), "{out}");
    // the operator's [network] (narrowed pool, short lease) stands
    let n = cfg.network.as_ref().unwrap();
    assert_eq!(
        n.pool(),
        (Ipv4Addr::new(10, 23, 7, 20), Ipv4Addr::new(10, 23, 7, 200))
    );
    assert_eq!(n.lease_secs, 600);
    assert_eq!(cfg.hosts.len(), 1);
    assert_eq!(cfg.hosts[0].cpu_overcommit, 3.0);
    // re-joining: the CA being joined now wins over the old one
    assert_eq!(cfg.ca_url.as_deref(), Some("https://ca-box:23443"));
}

#[test]
fn force_takes_the_clusters_box_id_over_a_hand_edited_one() {
    let edited = LIVED_IN.replace("box = 7\nunderlay", "box = 3\nunderlay");
    let out = regenerate(
        &edited,
        &template(7, Some("https://ca-box:23443")),
        true,
        true,
        7,
    )
    .unwrap();
    assert_eq!(load(&out).mesh.unwrap().box_id, 7);
}

#[test]
fn force_on_a_box_whose_mesh_has_no_box_line_still_records_it() {
    let no_box = LIVED_IN.replace("box = 7\nunderlay", "underlay");
    let out = regenerate(&no_box, &template(7, None), false, true, 7).unwrap();
    let cfg = load(&out);
    assert_eq!(cfg.mesh.as_ref().unwrap().box_id, 7);
    let peer_boxes: Vec<u8> = cfg.mesh.unwrap().peers.iter().map(|p| p.box_id).collect();
    assert_eq!(
        peer_boxes,
        [0, 1],
        "the peers' box lines are not the one rewritten"
    );
    // not joining: the kept ca_url is the operator's
    assert_eq!(cfg.ca_url.as_deref(), Some("https://old-ca:23443"));
}

#[test]
fn force_regenerates_an_advertise_url_whose_name_is_dead() {
    let out = regenerate(LIVED_IN, &template(7, None), false, false, 7).unwrap();
    assert_eq!(
        load(&out).advertise_url.as_deref(),
        Some("https://joiner.example:23443")
    );
}

#[test]
fn force_over_an_unparsable_config_is_an_error_for_the_caller() {
    assert!(regenerate("box = [", &template(7, None), false, true, 7).is_err());
}

/// The CA side of `bootstrap --join --force`: a new key, a new request,
/// and the approval hands back the id the host had, not the lowest free
/// one and not the `--box` it asks for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_force_rekeys_but_keeps_the_box_id() {
    let ca = CaBox::start().await;
    // someone else takes 1 first, so the joiner's id is not just "lowest"
    let (filler, _) = super::support::box_req("filler", None);
    ca.enroll.submit(filler).await.unwrap();
    let j = Joiner::new();
    let run = |want: Option<u8>, force: bool| {
        let (url, fp) = (ca.url.clone(), ca.ca_fingerprint.clone());
        let (paths, state) = (super::support::paths_in(j.dir.path()), j.state_dir.clone());
        tokio::task::spawn_blocking(move || {
            join::join(&JoinOptions {
                ca_url: &url,
                expect_fingerprint: Some(&fp),
                hostname: "joiner",
                box_id: want,
                api_url: "https://joiner:23443",
                force,
                paths: &paths,
                state_dir: &state,
            })
        })
    };
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let first = run(None, false).await.unwrap().unwrap();
    approver.await.unwrap();
    assert_eq!(first.box_id, 2);
    let key1 = std::fs::read_to_string(&j.paths.server_key).unwrap();

    // the filler goes away; a fresh request would now get 1
    ca.enroll
        .decide("filler", crate::enroll::Decision::Deny)
        .await
        .unwrap();
    let approver = tokio::spawn(approve_when_pending(ca.enroll.clone(), "joiner".into()));
    let again = run(Some(9), true).await.unwrap().unwrap();
    approver.await.unwrap();
    assert_eq!(again.box_id, first.box_id);
    assert_ne!(
        std::fs::read_to_string(&j.paths.server_key).unwrap(),
        key1,
        "--force made a new key"
    );
    // and the regenerated config records that id
    let out = regenerate(
        LIVED_IN,
        &template(again.box_id, Some(&ca.url)),
        true,
        true,
        again.box_id,
    )
    .unwrap();
    assert_eq!(load(&out).mesh.unwrap().box_id, first.box_id);
}
