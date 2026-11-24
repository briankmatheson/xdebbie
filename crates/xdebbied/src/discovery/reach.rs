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
    let Ok(out) = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show", "scope", "global"])
        .output()
    else {
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
        IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() || v4.is_multicast())
        }
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()),
    }
}

/// `url` as announced, unless its host is a name that does not resolve here;
/// then the same URL with the host replaced by the first usable candidate.
/// Candidates are tried in order: pass the packet's source address first,
/// then what the peer announced.
#[cfg(test)]
pub fn reachable_url(url: &str, candidates: impl IntoIterator<Item = IpAddr>) -> String {
    reachable_url_with(url, candidates, resolves)
}

/// [`reachable_url`] asking `resolves` whether a name resolves here, in
/// place of the system resolver.
pub fn reachable_url_with(
    url: &str,
    candidates: impl IntoIterator<Item = IpAddr>,
    resolves: impl Fn(&str) -> bool,
) -> String {
    let Some(parts) = split_url(url) else {
        return url.to_string();
    };
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
/// `resolves` is the resolver to ask ([`resolves`] in the daemon).
pub async fn reachable_url_async(
    url: String,
    candidates: Vec<IpAddr>,
    resolves: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> String {
    if !needs_lookup(&url) {
        return reachable_url_with(&url, candidates, |_| false);
    }
    match tokio::task::spawn_blocking(move || reachable_url_with(&url, candidates, &*resolves))
        .await
    {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "resolver task failed");
            String::new()
        }
    }
}

/// Does `url` name its host (rather than give an address), so that
/// [`reachable_url`] has to ask a resolver?
pub fn needs_lookup(url: &str) -> bool {
    split_url(url).is_some_and(|p| p.host.parse::<IpAddr>().is_err())
}

/// The system resolver: does `host` resolve to anything at all?
pub fn resolves(host: &str) -> bool {
    (host, 0u16)
        .to_socket_addrs()
        .map(|mut a| a.next().is_some())
        .unwrap_or(false)
}

/// Does `host` resolve to a usable address within `budget`? `getaddrinfo`
/// has no timeout of its own and a dead resolver can sit for a minute, so
/// the lookup runs on its own thread and is abandoned when the budget is
/// spent. Link-local and loopback results do not count: a name that only
/// yields `fe80::…` is no use to another box.
pub fn resolves_within(host: &str, budget: std::time::Duration) -> bool {
    resolves_within_with(host, budget, system_lookup)
}

/// The system resolver's addresses for `host` (`getaddrinfo`).
pub fn system_lookup(host: &str) -> std::io::Result<Vec<IpAddr>> {
    Ok((host, 0u16).to_socket_addrs()?.map(|a| a.ip()).collect())
}

/// [`resolves_within`] with `lookup` in place of the system resolver.
pub fn resolves_within_with(
    host: &str,
    budget: std::time::Duration,
    lookup: impl FnOnce(&str) -> std::io::Result<Vec<IpAddr>> + Send + 'static,
) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let host = host.to_string();
    std::thread::spawn(move || {
        let ok = lookup(&host)
            .map(|ips| ips.into_iter().any(|ip| usable(ip) && !is_link_local(ip)))
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
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
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
    let authority = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
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
    Some(UrlParts {
        scheme,
        host,
        port,
        rest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

    /// A resolver that knows exactly one name. The system one is no use in
    /// a test: a `search` domain with a wildcard record (or a container
    /// with no DNS at all) changes what "unresolvable" means.
    fn known(host: &str) -> bool {
        host == "known.example"
    }

    /// The same as a `getaddrinfo` stand-in: literals parse, a few names
    /// have fixed answers, one never answers, the rest fail.
    fn lookup(host: &str) -> std::io::Result<Vec<IpAddr>> {
        if let Ok(ip) = host.parse() {
            return Ok(vec![ip]);
        }
        let ips = |v: &[&str]| Ok(v.iter().map(|s| s.parse().unwrap()).collect());
        match host {
            "known.example" => ips(&["192.0.2.9"]),
            "loopback.example" => ips(&["127.0.1.1"]),
            "linklocal.example" => ips(&["fe80::1", "169.254.0.1"]),
            "mixed.example" => ips(&["fe80::1", "192.0.2.10"]),
            "slow.example" => {
                std::thread::sleep(Duration::from_secs(5));
                ips(&["192.0.2.11"])
            }
            _ => Err(std::io::Error::other("no such host")),
        }
    }

    fn within(host: &str) -> bool {
        resolves_within_with(host, Duration::from_millis(500), lookup)
    }

    #[test]
    fn resolves_within_budget() {
        assert!(within("192.0.2.7"));
        assert!(within("known.example"));
        assert!(
            within("mixed.example"),
            "one usable address among link-local ones is enough"
        );
        assert!(
            !within("127.0.0.1"),
            "loopback is not reachable from elsewhere"
        );
        assert!(!within("loopback.example"));
        assert!(
            !within("fe80::1"),
            "link-local is not reachable from elsewhere"
        );
        assert!(!within("linklocal.example"));
        assert!(!within("no-such-host.invalid"));
        assert!(!within(""));
    }

    #[test]
    fn slow_resolver_is_abandoned() {
        let t = std::time::Instant::now();
        assert!(!within("slow.example"), "answer came after the budget");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn unresolvable_name_becomes_address() {
        let r = |u| reachable_url_with(u, [IP], known);
        assert_eq!(
            r("https://no-such-host.invalid:23443"),
            "https://192.0.2.7:23443"
        );
        assert_eq!(
            r("https://no-such-host.invalid:23443/v1/x"),
            "https://192.0.2.7:23443/v1/x"
        );
        assert_eq!(r("http://no-such-host.invalid"), "http://192.0.2.7");
        assert_eq!(
            r("https://user@no-such-host.invalid:23443"),
            "https://192.0.2.7:23443"
        );
    }

    #[test]
    fn resolvable_and_literal_hosts_untouched() {
        let r = |u| reachable_url_with(u, [IP], known);
        assert_eq!(
            r("https://known.example:23443"),
            "https://known.example:23443"
        );
        assert_eq!(r("https://10.0.0.1:23443"), "https://10.0.0.1:23443");
        assert_eq!(r("https://[::1]:23443"), "https://[::1]:23443");
        assert_eq!(r("not a url"), "not a url");
    }

    #[test]
    fn literals_never_ask_the_resolver() {
        let asked = std::cell::Cell::new(false);
        let r = |u| {
            reachable_url_with(u, [IP], |_| {
                asked.set(true);
                false
            })
        };
        r("https://10.0.0.1:23443");
        r("https://[2001:db8::1]:23443");
        assert!(!asked.get());
        assert!(!needs_lookup("https://10.0.0.1:23443"));
        assert!(!needs_lookup("https://[2001:db8::1]:23443"));
        assert!(needs_lookup("https://node0.example.net:23443"));
        assert!(!needs_lookup("garbage"));
    }

    #[test]
    fn unusable_candidates_skipped() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let ll = IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1));
        let mc = IpAddr::V4(Ipv4Addr::new(239, 0, 0, 42));
        let any = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let u = "https://no-such-host.invalid:23443";
        assert_eq!(
            reachable_url_with(u, [lo, ll, mc, any, IP], known),
            "https://192.0.2.7:23443"
        );
        assert_eq!(reachable_url_with(u, [lo], known), u);
        assert_eq!(reachable_url_with(u, [], known), u);
    }

    #[test]
    fn source_address_wins_over_announced() {
        let announced = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
        let src: SocketAddr = "192.0.2.7:23442".parse().unwrap();
        let candidates = source_ip(src).into_iter().chain([announced]);
        assert_eq!(
            reachable_url_with("https://n.invalid:23443", candidates, known),
            "https://192.0.2.7:23443"
        );
    }

    #[test]
    fn v6_rewrite_is_bracketed() {
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(
            reachable_url_with("https://no-such-host.invalid:23443", [v6], known),
            "https://[2001:db8::1]:23443"
        );
    }

    #[tokio::test]
    async fn async_variant_matches_and_skips_lookup_for_literals() {
        let r: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> = std::sync::Arc::new(known);
        assert_eq!(
            reachable_url_async("https://nope.invalid:23443".into(), vec![IP], r.clone()).await,
            "https://192.0.2.7:23443"
        );
        assert_eq!(
            reachable_url_async("https://known.example:23443".into(), vec![IP], r).await,
            "https://known.example:23443"
        );
        let panics: std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync> =
            std::sync::Arc::new(|h| panic!("asked about {h}"));
        assert_eq!(
            reachable_url_async("https://10.0.0.1:23443".into(), vec![IP], panics).await,
            "https://10.0.0.1:23443"
        );
    }

    #[test]
    fn split() {
        let p = split_url("https://user@h.example:23443/v1").unwrap();
        assert_eq!(
            (p.scheme, p.host, p.port, p.rest),
            ("https", "h.example", Some("23443"), "/v1")
        );
        let p = split_url("https://[::1]:23443").unwrap();
        assert_eq!((p.host, p.port, p.rest), ("::1", Some("23443"), ""));
        assert!(split_url("not a url").is_none());
        assert!(split_url("https://:23443").is_none());
        assert!(split_url("https://[::1").is_none());
    }

    #[test]
    fn source_ip_unmaps_v4() {
        let s: SocketAddr = "[::ffff:192.0.2.7]:23442".parse().unwrap();
        assert_eq!(source_ip(s), Some(IP));
        let s: SocketAddr = "127.0.0.1:23442".parse().unwrap();
        assert_eq!(source_ip(s), None);
    }

    #[test]
    fn local_addrs_has_no_loopback() {
        for ip in local_addrs("239.0.0.42:23442") {
            assert!(usable(ip), "{ip}");
        }
    }

    /// The system resolver, for a box whose DNS is known to behave: no
    /// wildcard `search` domain, `localhost` in /etc/hosts.
    /// `XDEBBIE_TEST_DNS=1 cargo test -p xdebbied -- --ignored system_resolver`
    #[test]
    #[ignore = "needs a real resolver; XDEBBIE_TEST_DNS=1"]
    fn system_resolver() {
        if std::env::var_os("XDEBBIE_TEST_DNS").is_none() {
            eprintln!("XDEBBIE_TEST_DNS unset; skipping");
            return;
        }
        let b = Duration::from_secs(2);
        assert!(resolves_within("192.0.2.7", b));
        assert!(!resolves_within("127.0.0.1", b));
        assert!(!resolves_within("no-such-host.invalid", b));
        assert_eq!(
            reachable_url("https://no-such-host.invalid:23443", [IP]),
            "https://192.0.2.7:23443"
        );
        assert_eq!(
            reachable_url("https://localhost:23443", [IP]),
            "https://localhost:23443"
        );
    }
}
