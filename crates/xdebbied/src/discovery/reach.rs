// SPDX-License-Identifier: GPL-3.0-or-later
//! Reaching a peer by address when its name means nothing here.
//!
//! A box announces the URL from its `advertise_url`, which is built from its
//! hostname. That name resolves on the box that owns it and, on a fresh LAN
//! with no DNS and no `/etc/hosts` entries, nowhere else. So an `ANNOUNCE`
//! also carries the sender's addresses, the receiver notes the packet's
//! source, and when the URL's host does not resolve locally the host is
//! swapped for an address. API certificates list every local address as a
//! SAN, so TLS keeps verifying.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};

use tracing::warn;

/// Addresses to announce: the one the multicast route leaves by first, then
/// every other global-scope IPv4 address on the box.
pub fn local_addrs(multicast_addr: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = vec![];
    if let Some(ip) = egress_addr(multicast_addr) {
        out.push(ip);
    }
    for ip in global_ipv4_addrs() {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// The local address a packet to `target` would leave from. A connected UDP
/// socket sends nothing; it only asks the kernel to pick the route.
fn egress_addr(target: &str) -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect(target).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    usable(ip).then_some(ip)
}

/// Every IPv4 address a multicast group can be joined on: the global-scope
/// ones. Order follows `ip addr`.
pub fn local_v4_interfaces() -> Vec<std::net::Ipv4Addr> {
    global_ipv4_addrs()
        .into_iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect()
}

/// Every global-scope IPv4 address on the host, from `ip -4 -o addr`.
fn global_ipv4_addrs() -> Vec<IpAddr> {
    let Ok(out) = std::process::Command::new("ip").args(["-4", "-o", "addr", "show", "scope", "global"]).output() else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            f.position(|w| w == "inet")?;
            f.next()?.split('/').next()?.parse::<IpAddr>().ok()
        })
        .filter(|ip| usable(*ip))
        .collect()
}

fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() || v4.is_multicast()),
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()),
    }
}

/// `url` as announced, unless its host is a name that does not resolve here;
/// then the same URL with the host replaced by the first usable candidate.
/// Candidates are tried in order: pass the packet's source address first,
/// then what the peer announced.
pub fn reachable_url(url: &str, candidates: impl IntoIterator<Item = IpAddr>) -> String {
    let Some(parts) = split_url(url) else { return url.to_string() };
    if parts.host.parse::<IpAddr>().is_ok() || resolves(parts.host) {
        return url.to_string();
    }
    match candidates.into_iter().find(|ip| usable(*ip)) {
        Some(ip) => {
            let rewritten = parts.with_host(ip);
            warn!(host = parts.host, %ip, "peer's hostname does not resolve here; using its address ({rewritten})");
            rewritten
        }
        None => url.to_string(),
    }
}

/// Same as [`reachable_url`] for the daemon: the resolver call runs off the
/// async runtime so a slow DNS timeout does not stall announces.
pub async fn reachable_url_async(url: String, candidates: Vec<IpAddr>) -> String {
    match tokio::task::spawn_blocking(move || reachable_url(&url, candidates)).await {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "resolver task failed");
            String::new()
        }
    }
}

fn resolves(host: &str) -> bool {
    (host, 0u16).to_socket_addrs().map(|mut a| a.next().is_some()).unwrap_or(false)
}

/// Does `host` resolve to a usable address within `budget`? `getaddrinfo`
/// has no timeout of its own and a dead resolver can sit for a minute, so
/// the lookup runs on its own thread and is abandoned when the budget is
/// spent. Link-local and loopback results do not count: a name that only
/// yields `fe80::…` is no use to another box.
pub fn resolves_within(host: &str, budget: std::time::Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let host = host.to_string();
    std::thread::spawn(move || {
        let ok = (host.as_str(), 0u16)
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).any(|ip| usable(ip) && !is_link_local(ip)))
            .unwrap_or(false);
        let _ = tx.send(ok);
    });
    rx.recv_timeout(budget).unwrap_or(false)
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// The address this box announces first: where the multicast route leaves
/// from, else the first global IPv4 on the box.
pub fn announced_addr(multicast_addr: &str) -> Option<IpAddr> {
    local_addrs(multicast_addr).into_iter().next()
}

/// The source address of a received datagram as a candidate.
pub fn source_ip(src: SocketAddr) -> Option<IpAddr> {
    let ip = match src.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    usable(ip).then_some(ip)
}

struct UrlParts<'a> {
    scheme: &'a str,
    host: &'a str,
    port: Option<&'a str>,
    rest: &'a str,
}

impl UrlParts<'_> {
    fn with_host(&self, ip: IpAddr) -> String {
        let host = match ip {
            IpAddr::V6(v6) => format!("[{v6}]"),
            IpAddr::V4(v4) => v4.to_string(),
        };
        match self.port {
            Some(p) => format!("{}://{}:{}{}", self.scheme, host, p, self.rest),
            None => format!("{}://{}{}", self.scheme, host, self.rest),
        }
    }
}

/// `scheme://host[:port][/rest]`, with `[v6]` literals. No `url` crate for
/// one field.
fn split_url(url: &str) -> Option<UrlParts<'_>> {
    let (scheme, after) = url.split_once("://")?;
    let (authority, rest) = match after.find('/') {
        Some(i) => after.split_at(i),
        None => (after, ""),
    };
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (h, tail) = stripped.split_once(']')?;
        (h, tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
            _ => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(UrlParts { scheme, host, port, rest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

    #[test]
    fn resolves_within_budget() {
        let b = std::time::Duration::from_secs(2);
        assert!(resolves_within("192.0.2.7", b));
        assert!(!resolves_within("127.0.0.1", b), "loopback is not reachable from elsewhere");
        assert!(!resolves_within("fe80::1", b), "link-local is not reachable from elsewhere");
        assert!(!resolves_within("no-such-host.invalid", b));
        assert!(!resolves_within("", b));
    }

    #[test]
    fn unresolvable_name_becomes_address() {
        let u = reachable_url("https://no-such-host.invalid:7443", [IP]);
        assert_eq!(u, "https://192.0.2.7:7443");
        let u = reachable_url("https://no-such-host.invalid:7443/v1/x", [IP]);
        assert_eq!(u, "https://192.0.2.7:7443/v1/x");
        let u = reachable_url("http://no-such-host.invalid", [IP]);
        assert_eq!(u, "http://192.0.2.7");
    }

    #[test]
    fn resolvable_and_literal_hosts_untouched() {
        assert_eq!(reachable_url("https://localhost:7443", [IP]), "https://localhost:7443");
        assert_eq!(reachable_url("https://10.0.0.1:7443", [IP]), "https://10.0.0.1:7443");
        assert_eq!(reachable_url("https://[::1]:7443", [IP]), "https://[::1]:7443");
    }

    #[test]
    fn unusable_candidates_skipped() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(reachable_url("https://no-such-host.invalid:7443", [lo, IP]), "https://192.0.2.7:7443");
        assert_eq!(reachable_url("https://no-such-host.invalid:7443", [lo]), "https://no-such-host.invalid:7443");
        assert_eq!(reachable_url("https://no-such-host.invalid:7443", []), "https://no-such-host.invalid:7443");
    }

    #[test]
    fn v6_rewrite_is_bracketed() {
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(reachable_url("https://no-such-host.invalid:7443", [v6]), "https://[2001:db8::1]:7443");
    }

    #[test]
    fn split() {
        let p = split_url("https://user@h.example:7443/v1").unwrap();
        assert_eq!((p.scheme, p.host, p.port, p.rest), ("https", "h.example", Some("7443"), "/v1"));
        let p = split_url("https://[::1]:7443").unwrap();
        assert_eq!((p.host, p.port, p.rest), ("::1", Some("7443"), ""));
        assert!(split_url("not a url").is_none());
    }

    #[test]
    fn source_ip_unmaps_v4() {
        let s: SocketAddr = "[::ffff:192.0.2.7]:7442".parse().unwrap();
        assert_eq!(source_ip(s), Some(IP));
        let s: SocketAddr = "127.0.0.1:7442".parse().unwrap();
        assert_eq!(source_ip(s), None);
    }

    #[test]
    fn local_addrs_has_no_loopback() {
        for ip in local_addrs("239.0.0.42:7442") {
            assert!(usable(ip), "{ip}");
        }
    }
}
