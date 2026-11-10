// SPDX-License-Identifier: GPL-3.0-or-later
//! The one nftables table the mesh owns (section 2.6). Rendered as text and
//! applied atomically with `nft -f`; the manager only re-applies when the
//! rendered text changes.

use super::model::{MeshSpec, NFT_TABLE};

/// Render the table. Geneve traffic is only accepted on the underlay
/// interface, whether or not `allow_plain_underlay` is set: that flag
/// relaxes the route check, not which interface may carry UDP 6081.
pub fn render(spec: &MeshSpec) -> String {
    format!(
        "table inet {table} {{\n  chain input {{\n    type filter hook input priority -10; policy accept;\n    udp dport {port} iifname != {iif} counter drop\n  }}\n}}\n",
        table = NFT_TABLE,
        port = spec.dstport,
        iif = quote(&spec.underlay),
    )
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(name: &str) -> String {
        std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn golden_wg_underlay() {
        let s = MeshSpec { underlay: "wg-flock".into(), ..Default::default() };
        assert_eq!(render(&s), golden("nft-wg-flock.golden"));
    }

    #[test]
    fn golden_plain_underlay() {
        let s = MeshSpec { underlay: "ul0".into(), allow_plain_underlay: true, dstport: 6081, ..Default::default() };
        assert_eq!(render(&s), golden("nft-plain-ul0.golden"));
    }

    #[test]
    fn custom_port_appears() {
        let s = MeshSpec { dstport: 7000, ..Default::default() };
        assert!(render(&s).contains("udp dport 7000 iifname != \"wg-flock\""));
    }
}
