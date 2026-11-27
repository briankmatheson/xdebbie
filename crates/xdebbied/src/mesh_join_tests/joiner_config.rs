// SPDX-License-Identifier: GPL-3.0-or-later
//! The joiner's `[network]` (config_gen::network_block): the cluster's
//! overlay copied whole, this box's own /24 as gateway and pool
//! (BOOTSTRAP.md "Joining", docs/network/addresses-and-dhcp.md).

use std::net::Ipv4Addr;

use super::support::{CLUSTER_NETWORK, cluster_status};
use crate::{
    bootstrap::{
        config_gen::{generate_config_for, network_block},
        libvirt_setup::{LibvirtResources, NetworkInfo},
    },
    dhcp::NetworkConfig,
};

fn parse(block: &str) -> NetworkConfig {
    let t: toml::Table = toml::from_str(block).unwrap_or_else(|e| panic!("{e}\n{block}"));
    t["network"].clone().try_into().unwrap()
}

fn ip(s: &str) -> Ipv4Addr {
    s.parse().unwrap()
}

#[test]
fn the_joiner_copies_every_cluster_wide_setting() {
    let cluster = cluster_status(CLUSTER_NETWORK);
    let (block, warnings) = network_block(&cluster, 4, "virbr-xdebbie");
    assert!(warnings.is_empty(), "{warnings:?}");
    let n = parse(&block);
    n.validate().unwrap();
    // same overlay, same names, same upstreams, same reservations, same time
    assert_eq!(n.cidr, cluster.cidr);
    assert_eq!(n.domain.as_deref(), Some("vm.example"));
    assert_eq!(n.upstream_dns, [ip("192.168.1.1"), ip("192.168.1.2")]);
    assert_eq!(n.reserved, [ip("10.23.0.9")]);
    assert_eq!(n.reserve_from_dns.len(), 1);
    assert_eq!(n.reserve_from_dns[0].zone, "mesh.example.org");
    assert_eq!(n.reserve_from_dns[0].server, "10.23.0.1");
    assert_eq!(n.ntp, [ip("10.23.0.1"), ip("192.168.1.123")]);
    assert_eq!(n.lease_secs, 7200);
    assert_eq!(n.bridge.as_deref(), Some("virbr-xdebbie"));
    // its own /24: its bridge is gateway (and DNS, since the cluster's DNS
    // was only box 0's bridge), its VMs .2-.254
    assert_eq!(n.gateway, Some(ip("10.23.4.1")));
    assert_eq!(n.dns, [ip("10.23.4.1")]);
    assert_eq!(n.pool(), (ip("10.23.4.2"), ip("10.23.4.254")));
}

#[test]
fn a_real_cluster_dns_server_is_kept_and_absent_options_stay_absent() {
    let mut cluster = cluster_status(CLUSTER_NETWORK);
    cluster.dns = vec![ip("10.23.0.53"), ip("10.23.0.54")];
    cluster.ntp.clear();
    cluster.domain = None;
    cluster.reserve_from_dns.clear();
    let (block, _) = network_block(&cluster, 1, "br");
    let n = parse(&block);
    assert_eq!(n.dns, [ip("10.23.0.53"), ip("10.23.0.54")]);
    assert!(n.ntp.is_empty() && n.domain.is_none() && n.reserve_from_dns.is_empty());
    assert!(
        !block.contains("ntp =") && !block.contains("domain ="),
        "{block}"
    );
}

#[test]
fn every_joiner_gets_a_pool_disjoint_from_every_other_box() {
    let cluster = cluster_status(CLUSTER_NETWORK);
    let (ca_start, ca_end) = (cluster.pool_start, cluster.pool_end);
    let mut pools = vec![(u32::from(ca_start), u32::from(ca_end))];
    for b in 1..=40u8 {
        let (block, warnings) = network_block(&cluster, b, "br");
        assert!(warnings.is_empty(), "box {b}: {warnings:?}");
        let (s, e) = parse(&block).pool();
        pools.push((u32::from(s), u32::from(e)));
    }
    for (i, a) in pools.iter().enumerate() {
        for b in &pools[i + 1..] {
            assert!(a.1 < b.0 || b.1 < a.0, "pools {a:?} and {b:?} overlap");
        }
    }
}

#[test]
fn a_cluster_pool_reaching_into_the_joiners_slash_24_is_flagged() {
    let mut cluster = cluster_status(CLUSTER_NETWORK);
    // box 0 set up by hand, allocating from the whole /16
    cluster.pool_start = ip("10.23.0.10");
    cluster.pool_end = ip("10.23.255.254");
    let (block, warnings) = network_block(&cluster, 3, "br");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("overlaps"), "{}", warnings[0]);
    assert!(
        warnings[0].contains("10.23.0.2-10.23.0.254"),
        "says what box 0 should use: {}",
        warnings[0]
    );
    // the warning rides along in the file, which still parses
    assert!(block.contains("# WARNING:"), "{block}");
    parse(&block);
    // touching just the first or the last address counts
    for (s, e) in [("10.23.3.254", "10.23.4.10"), ("10.23.2.0", "10.23.3.2")] {
        cluster.pool_start = ip(s);
        cluster.pool_end = ip(e);
        assert_eq!(network_block(&cluster, 3, "br").1.len(), 1, "{s}-{e}");
    }
    // a pool in someone else's /24 is not this joiner's problem
    cluster.pool_start = ip("10.23.7.2");
    cluster.pool_end = ip("10.23.7.254");
    assert!(network_block(&cluster, 3, "br").1.is_empty());
}

#[test]
fn a_joiner_on_an_overlay_without_room_gets_no_usable_pool() {
    let mut cluster = cluster_status(CLUSTER_NETWORK);
    cluster.cidr = "10.23.0.0/20".parse().unwrap();
    let (_, warnings) = network_block(&cluster, 16, "br");
    assert!(
        warnings.iter().any(|w| w.contains("room for 16 box")),
        "{warnings:?}"
    );
}

#[test]
fn a_generated_joiner_config_has_the_ca_url_box_id_and_its_own_slice() {
    let resources = LibvirtResources {
        networks: vec![NetworkInfo {
            name: "xdebbie".into(),
            bridge: "virbr-xdebbie".into(),
        }],
        pools: vec![],
    };
    let cluster = cluster_status(CLUSTER_NETWORK);
    let g = generate_config_for(
        &resources,
        "qemu:///system",
        None,
        7,
        Some("https://ca-box:23443"),
        "xdebbie",
        "xdebbie",
        Some(&cluster),
        "joiner.example",
        &[ip("9.9.9.9")],
    );
    let cfg: crate::config::Config = toml::from_str(&g).unwrap_or_else(|e| panic!("{e}\n{g}"));
    assert_eq!(cfg.ca_url.as_deref(), Some("https://ca-box:23443"));
    assert_eq!(cfg.mesh.as_ref().map(|m| m.box_id), Some(7));
    let n = cfg.network.unwrap();
    assert_eq!(n.pool(), (ip("10.23.7.2"), ip("10.23.7.254")));
    assert_eq!(n.gateway, Some(ip("10.23.7.1")));
    assert_eq!(n.ntp, cluster.ntp);
    // the cluster's upstreams win over this box's resolv.conf
    assert_eq!(n.upstream_dns, cluster.upstream_dns);
}
