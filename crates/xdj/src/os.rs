// SPDX-License-Identifier: GPL-3.0-or-later
//! The OS-native half of xdj: what to run on Linux, macOS and Windows to
//! bring the WireGuard link up, route the mesh into it, and point the mesh's
//! DNS zone at the mesh resolver — and how to undo it.
//!
//! Every platform's moves are built here as plain data ([`Cmd`]) and compiled
//! on every platform, so the Linux build type-checks and tests the macOS and
//! Windows plans too. The one picked at run time is [`Os::here`].
//!
//! xdj never installs WireGuard. [`Os::missing_wireguard`] names what is
//! absent and the caller exits 1.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Win,
}

/// One command: program, arguments, and whether a failure is fine (the
/// thing already exists, or is already gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub p: String,
    pub a: Vec<String>,
    pub soft: bool,
}

fn cmd(p: &str, a: &[&str]) -> Cmd {
    Cmd {
        p: p.into(),
        a: a.iter().map(|s| s.to_string()).collect(),
        soft: false,
    }
}
fn soft(p: &str, a: &[&str]) -> Cmd {
    Cmd {
        soft: true,
        ..cmd(p, a)
    }
}

impl std::fmt::Display for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let q = |s: &String| {
            if s.is_empty() || s.contains([' ', '\'', '"', '$', '|', '{']) {
                format!("'{s}'")
            } else {
                s.clone()
            }
        };
        write!(f, "{}", q(&self.p))?;
        for a in &self.a {
            write!(f, " {}", q(a))?;
        }
        Ok(())
    }
}

/// Everything the plans need, already resolved from the conf.
#[derive(Debug, Clone)]
pub struct Link {
    /// Interface name (Linux), tunnel name (Windows), name-file stem (macOS).
    pub name: String,
    /// This box's underlay address, `10.22.<box>.1`.
    pub me: String,
    /// The hub's underlay address.
    pub hub: String,
    pub hub_pubkey: String,
    /// `host:port`, or empty for a hub that cannot be dialled (it dials us).
    pub endpoint: String,
    pub port: u16,
    pub mtu: u16,
    pub keepalive: u16,
    /// Networks routed into the tunnel besides the underlay (`routes`).
    pub routes: Vec<String>,
    /// The mesh resolver, or empty.
    pub dns: String,
    /// The zone it answers for (`dns_domain`), or empty.
    pub domain: String,
    /// Where the private key is on disk.
    pub key_file: String,
}

impl Link {
    fn allowed_ips(&self) -> Vec<String> {
        std::iter::once(format!("{}/32", self.hub))
            .chain(self.routes.iter().cloned())
            .collect()
    }
    fn underlay_net(&self) -> String {
        let o: Vec<&str> = self.me.split('.').collect();
        format!(
            "{}.{}.0.0/16",
            o.first().unwrap_or(&"10"),
            o.get(1).unwrap_or(&"22")
        )
    }
}

pub const PROGRAM_FILES_WG: &str = r"C:\Program Files\WireGuard";

/// Marks files xdj wrote, so `down` removes only its own.
pub const MARK: &str = "# xdj";

impl Os {
    pub fn here() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Win
        } else {
            Os::Linux
        }
    }

    /// The `wg` tool (genkey, pubkey, set).
    pub fn wg(self) -> String {
        match self {
            Os::Win => format!(r"{PROGRAM_FILES_WG}\wg.exe"),
            _ => "wg".into(),
        }
    }

    /// What WireGuard pieces are missing, as a message to exit 1 with, or
    /// None. `on_path` answers "is this program on PATH?".
    pub fn missing_wireguard(
        self,
        on_path: impl Fn(&str) -> bool,
        exists: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let (need, hint): (Vec<String>, &str) = match self {
            Os::Linux => (
                vec!["wg".into()],
                "the kernel module and wireguard-tools (e.g. `apt install wireguard-tools`)",
            ),
            Os::Mac => (
                vec!["wg".into(), "wireguard-go".into()],
                "wireguard-tools, which brings wireguard-go (e.g. `brew install wireguard-tools`)",
            ),
            Os::Win => (
                vec![
                    format!(r"{PROGRAM_FILES_WG}\wireguard.exe"),
                    format!(r"{PROGRAM_FILES_WG}\wg.exe"),
                ],
                "WireGuard for Windows from https://www.wireguard.com/install/",
            ),
        };
        let gone: Vec<String> = need
            .into_iter()
            .filter(|n| {
                if self == Os::Win {
                    !exists(n)
                } else {
                    !on_path(n)
                }
            })
            .collect();
        (!gone.is_empty()).then(|| {
            format!("WireGuard is needed and was not found ({}). Install {hint}, then run xdj again. xdj does not install it.", gone.join(", "))
        })
    }

    /// Linux: create, key, address, peer, routes. One plan, run in order.
    pub fn linux_up(l: &Link) -> Vec<Cmd> {
        let n = l.name.as_str();
        let (port, mtu, ka) = (
            l.port.to_string(),
            l.mtu.to_string(),
            l.keepalive.to_string(),
        );
        let aips = l.allowed_ips().join(",");
        let mut v = vec![
            soft("ip", &["link", "add", n, "type", "wireguard"]),
            cmd("ip", &["link", "set", n, "alias", super::A]),
            cmd(
                "wg",
                &["set", n, "private-key", &l.key_file, "listen-port", &port],
            ),
            cmd(
                "ip",
                &["addr", "replace", &format!("{}/16", l.me), "dev", n],
            ),
            cmd("ip", &["link", "set", n, "mtu", &mtu, "up"]),
        ];
        let mut peer = vec!["set", n, "peer", &l.hub_pubkey];
        if !l.endpoint.is_empty() {
            peer.extend(["endpoint", &l.endpoint]);
        }
        peer.extend(["allowed-ips", &aips, "persistent-keepalive", &ka]);
        v.push(cmd("wg", &peer));
        // `add`, not `replace`: a box whose own bridge already carries one of
        // these networks keeps it, and the failure says so.
        for r in &l.routes {
            v.push(cmd("ip", &["route", "add", r, "dev", n]));
        }
        v
    }

    /// Linux, scoped DNS through systemd-resolved: the zone goes to the mesh
    /// resolver over this link, everything else is untouched; it goes away
    /// with the link.
    pub fn linux_dns(l: &Link) -> Vec<Cmd> {
        vec![
            cmd("resolvectl", &["dns", &l.name, &l.dns]),
            cmd(
                "resolvectl",
                &["domain", &l.name, &format!("~{}", l.domain)],
            ),
        ]
    }

    pub fn linux_down(l: &Link) -> Vec<Cmd> {
        vec![soft("ip", &["link", "del", &l.name])]
    }

    /// macOS, first half: wireguard-go makes a utun and writes its name to
    /// `<run>/<name>.name` (how wg-quick finds it too).
    pub fn mac_create(l: &Link, run: &str) -> (Vec<Cmd>, String) {
        let name_file = format!("{run}/{}.name", l.name);
        (
            vec![
                cmd("mkdir", &["-p", run]),
                cmd(
                    "env",
                    &[
                        &format!("WG_TUN_NAME_FILE={name_file}"),
                        "wireguard-go",
                        "utun",
                    ],
                ),
            ],
            name_file,
        )
    }

    /// macOS, second half, once the utun's real name is known.
    pub fn mac_up(l: &Link, utun: &str) -> Vec<Cmd> {
        let (port, mtu, ka) = (
            l.port.to_string(),
            l.mtu.to_string(),
            l.keepalive.to_string(),
        );
        let aips = l.allowed_ips().join(",");
        let mut wg = vec![
            "set",
            utun,
            "private-key",
            &l.key_file,
            "listen-port",
            &port,
            "peer",
            &l.hub_pubkey,
        ];
        if !l.endpoint.is_empty() {
            wg.extend(["endpoint", &l.endpoint]);
        }
        wg.extend(["allowed-ips", &aips, "persistent-keepalive", &ka]);
        let un = l.underlay_net();
        let mut v = vec![
            cmd("wg", &wg),
            cmd(
                "ifconfig",
                &[utun, "inet", &format!("{}/16", l.me), &l.me, "alias"],
            ),
            cmd("ifconfig", &[utun, "mtu", &mtu]),
            cmd("ifconfig", &[utun, "up"]),
            cmd(
                "route",
                &["-q", "-n", "add", "-inet", &un, "-interface", utun],
            ),
        ];
        for r in &l.routes {
            v.push(cmd(
                "route",
                &["-q", "-n", "add", "-inet", r, "-interface", utun],
            ));
        }
        v
    }

    /// macOS scoped DNS: a resolver(5) file for the zone. Path and body.
    pub fn mac_dns(l: &Link) -> (String, String) {
        (
            format!("/etc/resolver/{}", l.domain),
            format!(
                "{MARK}: the mesh zone, removed by `xdj down`\nnameserver {}\n",
                l.dns
            ),
        )
    }

    /// macOS down: removing wireguard-go's control socket makes it exit,
    /// and the utun and its routes go with it.
    pub fn mac_down(utun: &str, run: &str) -> Vec<Cmd> {
        vec![soft("rm", &["-f", &format!("{run}/{utun}.sock")])]
    }

    /// Windows: the tunnel service's config. `Table = off`: the service
    /// makes the adapter and its address, and xdj routes with netsh.
    pub fn win_conf(l: &Link, private_key: &str) -> String {
        let mut s = format!(
            "{MARK}: written by `xdj up`, removed by `xdj down`\n[Interface]\nPrivateKey = {}\nListenPort = {}\nAddress = {}/16\nMTU = {}\nTable = off\n\n[Peer]\nPublicKey = {}\nAllowedIPs = {}\nPersistentKeepalive = {}\n",
            private_key.trim(), l.port, l.me, l.mtu, l.hub_pubkey, l.allowed_ips().join(", "), l.keepalive
        );
        if !l.endpoint.is_empty() {
            s.push_str(&format!("Endpoint = {}\n", l.endpoint));
        }
        s
    }

    /// Windows up, after the conf is written to `conf`.
    pub fn win_up(l: &Link, conf: &str) -> Vec<Cmd> {
        let wgx = format!(r"{PROGRAM_FILES_WG}\wireguard.exe");
        // Only SYSTEM (S-1-5-18) and Administrators (S-1-5-32-544) may read
        // the key; SIDs, so it works whatever the language.
        let mut v = vec![
            cmd(
                "icacls",
                &[
                    conf,
                    "/inheritance:r",
                    "/grant:r",
                    "*S-1-5-18:F",
                    "*S-1-5-32-544:F",
                ],
            ),
            cmd(&wgx, &["/installtunnelservice", conf]),
        ];
        for r in &l.routes {
            v.push(cmd(
                "netsh",
                &[
                    "interface",
                    "ipv4",
                    "add",
                    "route",
                    r,
                    &l.name,
                    "store=active",
                ],
            ));
        }
        v
    }

    /// Windows scoped DNS: an NRPT rule for the zone, tagged so `down`
    /// removes only ours.
    pub fn win_dns(l: &Link) -> Cmd {
        cmd(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Add-DnsClientNrptRule -Namespace '.{}' -NameServers '{}' -Comment 'xdj'",
                    l.domain, l.dns
                ),
            ],
        )
    }

    pub fn win_down(l: &Link) -> Vec<Cmd> {
        vec![
            soft("powershell", &["-NoProfile", "-NonInteractive", "-Command",
                "Get-DnsClientNrptRule | Where-Object Comment -eq 'xdj' | Remove-DnsClientNrptRule -Force"]),
            soft(&format!(r"{PROGRAM_FILES_WG}\wireguard.exe"), &["/uninstalltunnelservice", &l.name]),
        ]
    }

    /// The Windows service that carries tunnel `name`.
    pub fn win_service(name: &str) -> String {
        format!("WireGuardTunnel${name}")
    }
}

/// Did xdj write this file? (`down` only removes its own.)
pub fn ours(path: &str) -> bool {
    Path::new(path).exists()
        && std::fs::read_to_string(path)
            .map(|s| s.starts_with(MARK))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> Link {
        Link {
            name: "wg-flock".into(),
            me: "10.22.5.1".into(),
            hub: "10.22.0.1".into(),
            hub_pubkey: "HUBKEY=".into(),
            endpoint: "203.0.113.9:51820".into(),
            port: 51820,
            mtu: 1420,
            keepalive: 25,
            routes: vec!["10.23.0.0/16".into()],
            dns: "10.23.0.11".into(),
            domain: "mesh.example".into(),
            key_file: "/home/me/.config/xdj/wg.key".into(),
        }
    }

    fn lines(v: &[Cmd]) -> Vec<String> {
        v.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn linux_is_wireguard_and_routes_only() {
        let v = lines(&Os::linux_up(&link()));
        assert!(
            !v.iter()
                .any(|c| c.contains("geneve") || c.contains("bridge")),
            "{v:#?}"
        );
        assert!(v.contains(&"wg set wg-flock peer HUBKEY= endpoint 203.0.113.9:51820 allowed-ips 10.22.0.1/32,10.23.0.0/16 persistent-keepalive 25".into()), "{v:#?}");
        assert_eq!(v.last().unwrap(), "ip route add 10.23.0.0/16 dev wg-flock");
        assert_eq!(
            lines(&Os::linux_dns(&link())),
            [
                "resolvectl dns wg-flock 10.23.0.11",
                "resolvectl domain wg-flock ~mesh.example"
            ]
        );
    }

    #[test]
    fn no_endpoint_no_endpoint_arg() {
        let mut l = link();
        l.endpoint.clear();
        assert!(!lines(&Os::linux_up(&l))
            .iter()
            .any(|c| c.contains("endpoint")));
        assert!(!lines(&Os::mac_up(&l, "utun4"))
            .iter()
            .any(|c| c.contains("endpoint")));
        assert!(!Os::win_conf(&l, "k").contains("Endpoint"));
    }

    #[test]
    fn mac_uses_utun_ifconfig_route_and_a_resolver_file() {
        let (c, nf) = Os::mac_create(&link(), "/var/run/wireguard");
        assert_eq!(nf, "/var/run/wireguard/wg-flock.name");
        assert_eq!(
            lines(&c)[1],
            "env WG_TUN_NAME_FILE=/var/run/wireguard/wg-flock.name wireguard-go utun"
        );
        let v = lines(&Os::mac_up(&link(), "utun4"));
        assert!(
            v.contains(&"ifconfig utun4 inet 10.22.5.1/16 10.22.5.1 alias".into()),
            "{v:#?}"
        );
        assert!(v.contains(&"route -q -n add -inet 10.22.0.0/16 -interface utun4".into()));
        assert!(v.contains(&"route -q -n add -inet 10.23.0.0/16 -interface utun4".into()));
        let (p, body) = Os::mac_dns(&link());
        assert_eq!(p, "/etc/resolver/mesh.example");
        assert!(body.starts_with(MARK) && body.ends_with("nameserver 10.23.0.11\n"));
        assert_eq!(
            lines(&Os::mac_down("utun4", "/var/run/wireguard")),
            ["rm -f /var/run/wireguard/utun4.sock"]
        );
    }

    #[test]
    fn windows_conf_service_netsh_and_nrpt() {
        let c = Os::win_conf(&link(), "PRIV=\n");
        for want in [
            "PrivateKey = PRIV=\n",
            "Address = 10.22.5.1/16\n",
            "Table = off\n",
            "AllowedIPs = 10.22.0.1/32, 10.23.0.0/16\n",
            "Endpoint = 203.0.113.9:51820\n",
        ] {
            assert!(c.contains(want), "{want:?} not in\n{c}");
        }
        let v = lines(&Os::win_up(&link(), r"C:\ProgramData\xdj\wg-flock.conf"));
        assert!(
            v[1].ends_with(
                r"wireguard.exe' /installtunnelservice 'C:\ProgramData\xdj\wg-flock.conf'"
            ) || v[1].contains("/installtunnelservice"),
            "{v:#?}"
        );
        assert_eq!(
            v[2],
            "netsh interface ipv4 add route 10.23.0.0/16 wg-flock store=active"
        );
        assert!(Os::win_dns(&link()).to_string().contains("Add-DnsClientNrptRule -Namespace '.mesh.example' -NameServers '10.23.0.11' -Comment 'xdj'"));
        assert_eq!(Os::win_service("wg-flock"), "WireGuardTunnel$wg-flock");
    }

    #[test]
    fn missing_wireguard_is_a_message_never_an_install() {
        let none = |_: &str| false;
        for os in [Os::Linux, Os::Mac, Os::Win] {
            let m = os
                .missing_wireguard(none, none)
                .expect("nothing is installed");
            assert!(m.contains("does not install"), "{m}");
        }
        assert!(Os::Mac
            .missing_wireguard(|p| p == "wg", none)
            .unwrap()
            .contains("wireguard-go"));
        assert!(Os::Linux.missing_wireguard(|_| true, none).is_none());
        assert!(Os::Win.missing_wireguard(none, |_| true).is_none());
    }
}
