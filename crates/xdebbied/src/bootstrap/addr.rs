// SPDX-License-Identifier: GPL-3.0-or-later
//! Overlay addressing: one /16, one /24 per box.
//!
//! The overlay is a single L2 segment once boxes are meshed, so every box
//! shares the `cidr` (default `10.23.0.0/16`) and box `N` owns its third
//! octet: `10.23.N.0/24`. Its bridge sits at `10.23.N.1`, its VMs get
//! `10.23.N.2`–`10.23.N.254`, and the mesh's derived probe (`10.23.<box>.1`)
//! lands on the bridge by construction. Nothing is shared and nothing has
//! to be reserved: two boxes can never hand out the same address.
//!
//! A narrower `cidr` still works with fewer boxes: a /24 has room for box 0
//! only, a /20 for boxes 0–15.

use anyhow::{Result, bail};
use std::net::Ipv4Addr;

use crate::dhcp::model::Cidr;

/// Box `N`'s share of the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxBlock {
    /// This box's bridge address: the gateway and DNS its VMs are told.
    pub bridge: Ipv4Addr,
    /// First address handed to a VM.
    pub pool_start: Ipv4Addr,
    /// Last address handed to a VM.
    pub pool_end: Ipv4Addr,
}

impl BoxBlock {
    /// The block as a /24 (or the whole `cidr` when it is narrower).
    pub fn network(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.bridge) - 1)
    }
}

/// How many boxes `cidr` has room for: one /24 each.
pub fn capacity(cidr: &Cidr) -> u32 {
    if cidr.prefix > 24 { 0 } else { 1u32 << (24 - cidr.prefix) }
}

/// Box `box_id`'s block of `cidr`.
pub fn box_block(cidr: &Cidr, box_id: u8) -> Result<BoxBlock> {
    let cap = capacity(cidr);
    if cap == 0 {
        bail!("overlay {cidr} is narrower than a /24; use a /16 (10.23.0.0/16) so every box gets its own /24");
    }
    if u32::from(box_id) >= cap {
        bail!("overlay {cidr} has room for {cap} box(es), not box {box_id}; use a /16 (10.23.0.0/16)");
    }
    let net = u32::from(cidr.network) + u32::from(box_id) * 256;
    let last = u32::from(cidr.last_host());
    Ok(BoxBlock {
        bridge: Ipv4Addr::from(net + 1),
        pool_start: Ipv4Addr::from(net + 2),
        pool_end: Ipv4Addr::from((net + 254).min(last)),
    })
}

/// Same, from a `a.b.c.d/N` string (bootstrap's `--subnet`).
pub fn box_block_str(subnet: &str, box_id: u8) -> Result<BoxBlock> {
    let s = if subnet.contains('/') { subnet.to_string() } else { format!("{subnet}/24") };
    let cidr: Cidr = s.parse().map_err(|e| anyhow::anyhow!("network subnet {subnet:?}: {e}"))?;
    box_block(&cidr, box_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn each_box_owns_its_third_octet() {
        let c: Cidr = "10.23.0.0/16".parse().unwrap();
        assert_eq!(capacity(&c), 256);
        let b0 = box_block(&c, 0).unwrap();
        assert_eq!((b0.bridge, b0.pool_start, b0.pool_end), (ip("10.23.0.1"), ip("10.23.0.2"), ip("10.23.0.254")));
        let b1 = box_block(&c, 1).unwrap();
        assert_eq!((b1.bridge, b1.pool_start, b1.pool_end), (ip("10.23.1.1"), ip("10.23.1.2"), ip("10.23.1.254")));
        assert_eq!(b1.network(), ip("10.23.1.0"));
        let b253 = box_block(&c, 253).unwrap();
        assert_eq!(b253.bridge, ip("10.23.253.1"));
        // the bridge is where the mesh probes by default
        let spec = crate::mesh::model::MeshSpec::default();
        let p = crate::mesh::model::PeerSpec::new(7, ip("10.22.7.1"));
        assert_eq!(spec.probe_addr(&p), box_block(&c, 7).unwrap().bridge);
    }

    #[test]
    fn narrower_overlays_hold_fewer_boxes() {
        let c: Cidr = "192.168.100.0/24".parse().unwrap();
        assert_eq!(capacity(&c), 1);
        let b = box_block(&c, 0).unwrap();
        assert_eq!((b.bridge, b.pool_start, b.pool_end), (ip("192.168.100.1"), ip("192.168.100.2"), ip("192.168.100.254")));
        let e = box_block(&c, 1).unwrap_err().to_string();
        assert!(e.contains("room for 1 box"), "{e}");
        let c: Cidr = "10.9.0.0/20".parse().unwrap();
        assert_eq!(capacity(&c), 16);
        assert_eq!(box_block(&c, 15).unwrap().bridge, ip("10.9.15.1"));
        assert!(box_block(&c, 16).is_err());
        let c: Cidr = "10.9.9.0/28".parse().unwrap();
        assert!(box_block(&c, 0).unwrap_err().to_string().contains("narrower than a /24"));
    }

    #[test]
    fn last_block_stops_before_broadcast() {
        let c: Cidr = "10.23.0.0/16".parse().unwrap();
        let b = box_block(&c, 255).unwrap();
        assert_eq!(b.pool_end, ip("10.23.255.254"));
        assert!(box_block_str("nope/16", 0).is_err());
        assert_eq!(box_block_str("10.23.0.0/16", 2).unwrap().bridge, ip("10.23.2.1"));
    }
}
