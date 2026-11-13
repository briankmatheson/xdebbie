// SPDX-License-Identifier: GPL-3.0-or-later
//! This box's upstream resolvers, for `[network] upstream_dns`.
//!
//! VMs are told their box's bridge as DNS; the box DNATs port 53 there to
//! an upstream server (bootstrap/firewall.rs). The first box takes what it
//! resolves with itself; joiners copy the cluster's list so every VM, on
//! every box, asks the same server as box 0.

use std::net::Ipv4Addr;

/// Files with real upstream servers, most authoritative first. With
/// systemd-resolved, `/etc/resolv.conf` usually points at the stub
/// (`127.0.0.53`), which is no use from a VM; the resolved-managed file
/// lists the servers it forwards to.
const RESOLV_FILES: &[&str] = &["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"];

/// IPv4 nameservers this host uses, loopback excluded (a VM cannot reach
/// `127.0.0.53`). Empty when nothing usable was found.
pub fn upstream_resolvers() -> Vec<Ipv4Addr> {
    for f in RESOLV_FILES {
        if let Ok(text) = std::fs::read_to_string(f) {
            let found = parse_resolv_conf(&text);
            if !found.is_empty() {
                return found;
            }
        }
    }
    vec![]
}

/// `nameserver` lines of a resolv.conf: IPv4, not loopback, in order,
/// without duplicates.
pub fn parse_resolv_conf(text: &str) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = vec![];
    for line in text.lines() {
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        let mut f = line.split_whitespace();
        if f.next() != Some("nameserver") {
            continue;
        }
        if let Some(ip) = f.next().and_then(|s| s.parse::<Ipv4Addr>().ok())
            && !ip.is_loopback()
            && !ip.is_unspecified()
            && !out.contains(&ip)
        {
            out.push(ip);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_nameservers_only() {
        let text = "# generated\nsearch example.net\nnameserver 127.0.0.53  # stub\nnameserver 192.168.1.1\nnameserver 2600:4040::1\nnameserver 192.168.1.1\nnameserver 9.9.9.9 ; quad9\noptions edns0\n";
        let got: Vec<String> = parse_resolv_conf(text).iter().map(|ip| ip.to_string()).collect();
        assert_eq!(got, ["192.168.1.1", "9.9.9.9"]);
        assert!(parse_resolv_conf("nameserver 127.0.0.1\n").is_empty());
        assert!(parse_resolv_conf("").is_empty());
    }
}
