// SPDX-License-Identifier: GPL-3.0-or-later
//! xdj — slip into the xdebbie mesh without running the daemon, from Linux,
//! macOS or Windows.
//!
//! It brings up one WireGuard link to the hub (10.22.<me>.1/16, peer
//! 10.22.<hub>.1) and routes the VM network (`routes`, default
//! 10.23.0.0/16) into it; the hub forwards to its VMs when it runs with
//! `[network] mesh_routed = true`. No Geneve, no bridge: the same on every OS.
//! With `dns_domain` set, that zone (and only it) is resolved by `dns`.
//!
//!   xdj           toggle: up if the link is down, down if it is up
//!   xdj up/down   force a direction
//!   xdj key       print our public key (register it on the hub)
//!   xdj clear     forget our key and the hub's; the next `up` starts over
//!   xdj show      print the moves, make none
//!   xdj status    say whether the link is up
//!
//! The moves are each OS's own (os.rs):
//!
//! | | link | address, routes | scoped DNS |
//! |---|---|---|---|
//! | Linux | `ip link add … type wireguard`, `wg set` | `ip addr`, `ip route add` | `resolvectl dns/domain` |
//! | macOS | `wireguard-go utun`, `wg set` | `ifconfig`, `route add` | `/etc/resolver/<zone>` |
//! | Windows | `wireguard.exe /installtunnelservice` (`Table = off`) | `netsh … add route` | NRPT rule (`Add-DnsClientNrptRule`) |
//!
//! WireGuard itself is never installed: without it xdj says what is missing
//! and exits 1.
//!
//! Identity: it adopts this box's xdebbie WireGuard key if one is on disk
//! (/var/lib/xdebbie/wg.key), so the hub already knows us; otherwise it asks
//! for a key to paste (Enter mints a fresh one) and saves it beside the conf.
//! The hub's public key and endpoint are asked for once and kept in
//! ~/.config/xdj/xdj.conf. The endpoint prompt offers a guess read off `w`:
//! on a remote box, the address you are logged in from is usually the hub's.
//!
//! It is for boxes WITHOUT the daemon: where xdebbied is running the mesh is
//! the daemon's, and xdj refuses to do anything. Linux and macOS re-exec
//! under sudo, carrying your $HOME so it reads your conf, not root's;
//! Windows wants an elevated prompt.

use std::collections::BTreeMap as M;
use std::io::{Error, ErrorKind::Other, Result, Write};
use std::process::{Command as X, Stdio as Z};

mod os;

const A: &str = "xdebbie-mesh/v1"; // the ownership alias the daemon stamps

fn num<T: std::str::FromStr + Default>(s: String) -> T {
    s.trim().parse().unwrap_or_default()
}
fn e(m: impl Into<String>) -> Error {
    Error::new(Other, m.into())
}
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}
fn rooted() -> bool {
    env("XDJ_ROOT").is_some()
}
// $HOME, but under sudo the user half hands theirs over so root reads it too.
fn home() -> String {
    env("XDJ_HOME")
        .or_else(|| env("HOME"))
        .or_else(|| env("USERPROFILE"))
        .unwrap_or_else(|| "/root".into())
}
fn cpath() -> String {
    env("XDJ_CONF").unwrap_or_else(|| format!("{}/.config/xdj/xdj.conf", home()))
}
fn kpath() -> String {
    format!("{}/.config/xdj/wg.key", home())
}

/// A wg endpoint needs host:port. A bare host/IP (no colon) gets the wg port
/// appended; anything already carrying a colon (host:port, or IPv6) is left be.
fn ep_with_port(ep: &str, port: u16) -> String {
    if ep.is_empty() || ep.contains(':') {
        ep.to_string()
    } else {
        format!("{ep}:{port}")
    }
}

/// The likeliest hub address in `w -h` output: the IPv4 address most sessions
/// here were opened FROM, a public one before a private one. FROM is the
/// second or third column (a session without a tty has no TTY column); later
/// columns are the command line and are left alone.
fn from_w(out: &str) -> Option<String> {
    use std::net::Ipv4Addr;
    let mut seen: M<Ipv4Addr, usize> = M::new();
    for l in out.lines() {
        if let Some(ip) = l
            .split_whitespace()
            .skip(1)
            .take(2)
            .find_map(|t| t.parse::<Ipv4Addr>().ok())
        {
            if !(ip.is_loopback() || ip.is_unspecified() || ip.is_link_local()) {
                *seen.entry(ip).or_default() += 1;
            }
        }
    }
    seen.into_iter()
        .max_by_key(|(ip, n)| (!ip.is_private(), *n))
        .map(|(ip, _)| ip.to_string())
}

/// Guess the hub's endpoint from who is logged in (`w`), or None.
fn guess_endpoint() -> Option<String> {
    let o = r("w", &["-h", "-i"], None, false)
        .or_else(|_| r("w", &["-h"], None, false))
        .ok()?;
    from_w(&o)
}

/// Run `p a…`, feeding `i` on stdin; `soft` swallows a failure (link exists).
fn r(p: &str, a: &[&str], i: Option<&str>, soft: bool) -> Result<String> {
    let mut c = X::new(p);
    c.args(a)
        .stdin(if i.is_some() { Z::piped() } else { Z::null() })
        .stdout(Z::piped())
        .stderr(Z::piped());
    let mut ch = c.spawn().map_err(|x| e(format!("{p}: {x}")))?;
    if let (Some(s), Some(mut h)) = (i, ch.stdin.take()) {
        let _ = h.write_all(s.as_bytes());
    }
    let o = ch.wait_with_output()?;
    if o.status.success() || soft {
        return Ok(String::from_utf8_lossy(&o.stdout).into());
    }
    Err(e(format!(
        "{p} {}: {}",
        a.join(" "),
        String::from_utf8_lossy(&o.stderr).trim()
    )))
}

/// The pid of a running xdebbied, if there is one: a numeric entry under
/// `proc` whose `comm` is the daemon's name. World-readable, so no root needed.
fn daemon_pid(proc: &str) -> Option<u32> {
    std::fs::read_dir(proc).ok()?.flatten().find_map(|d| {
        let pid: u32 = d.file_name().to_str()?.parse().ok()?;
        let comm = std::fs::read_to_string(d.path().join("comm")).ok()?;
        (comm.trim() == "xdebbied").then_some(pid)
    })
}

/// Ask on stderr, read one line from stdin. Empty allowed.
fn ask(q: &str) -> String {
    eprint!("{q}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// Give `path` back to the invoking user when we are root.
fn mine(path: &str) {
    if rooted() {
        if let Some(u) = env("XDJ_USER") {
            let _ = r("chown", &[&format!("{u}:"), path], None, true);
        }
    }
}

fn save(path: &str, body: &str, mode: &str) -> Result<()> {
    if let Some(d) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(d)?;
        mine(&d.to_string_lossy());
    }
    std::fs::write(path, body)?;
    if !cfg!(windows) {
        let _ = r("chmod", &[mode, path], None, true);
    }
    mine(path);
    Ok(())
}

/// `key = val` reader (`#` comments, quotes stripped). Missing file = empty.
fn conf() -> M<String, String> {
    let mut m = M::new();
    if let Ok(t) = std::fs::read_to_string(cpath()) {
        for l in t.lines() {
            let l = l.split('#').next().unwrap_or("").trim();
            if let Some((k, v)) = l.split_once('=') {
                m.insert(k.trim().into(), v.trim().trim_matches('"').into());
            }
        }
    }
    m
}

/// Set one key in the conf, keeping the other lines; create the file if new.
fn conf_set(key: &str, val: &str) -> Result<()> {
    let p = cpath();
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let mut out = String::new();
    let mut hit = false;
    for l in old.lines() {
        let bare = l.split('#').next().unwrap_or("").trim();
        if bare
            .split_once('=')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false)
        {
            out.push_str(&format!("{key} = {val}\n"));
            hit = true;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !hit {
        out.push_str(&format!("{key} = {val}\n"));
    }
    save(&p, &out, "644")
}

/// Drop one key from the conf, keeping the other lines. True if it was there.
fn conf_del(key: &str) -> Result<bool> {
    let p = cpath();
    let Ok(old) = std::fs::read_to_string(&p) else {
        return Ok(false);
    };
    let is = |l: &str| {
        l.split('#')
            .next()
            .unwrap_or("")
            .split_once('=')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false)
    };
    if !old.lines().any(is) {
        return Ok(false);
    }
    let out: String = old
        .lines()
        .filter(|l| !is(l))
        .map(|l| format!("{l}\n"))
        .collect();
    save(&p, &out, "644")?;
    Ok(true)
}

fn pubkey_of(priv_: &str) -> Result<String> {
    Ok(r(
        &os::Os::here().wg(),
        &["pubkey"],
        Some(&format!("{}\n", priv_.trim())),
        false,
    )?
    .trim()
    .into())
}

fn dns_state() -> String {
    format!("{}/.config/xdj/dns.conn", home())
}

/// Does `server` answer DNS? A real query, not ping: the mesh resolver is a
/// LoadBalancer IP that never replies to ICMP but serves port 53 fine. Retries
/// briefly so a just-up overlay has time to prime its learned FDB.
fn dns_answers(server: &str) -> bool {
    use std::net::UdpSocket;
    use std::time::Duration;
    // Minimal DNS query: id, RD flag, one question for the root "." A IN.
    let q: [u8; 17] = [
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x00, 0x01,
    ];
    for _ in 0..4 {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            let _ = s.set_read_timeout(Some(Duration::from_millis(1200)));
            if s.send_to(&q, format!("{server}:53")).is_ok() {
                let mut buf = [0u8; 64];
                if s.recv_from(&mut buf).is_ok() {
                    return true;
                }
            }
        }
    }
    false
}

/// Undo the system-wide DNS an xdj from before routing set (nmcli):
/// hand the remembered connection its DHCP resolvers back.
fn dns_revert() {
    let Ok(c) = std::fs::read_to_string(dns_state()) else {
        return;
    };
    let conn = c.trim();
    if conn.is_empty() {
        return;
    }
    let m = |k: &str, v: &str| {
        let _ = r("nmcli", &["connection", "modify", conn, k, v], None, true);
    };
    m("ipv4.dns", "");
    m("ipv4.ignore-auto-dns", "no");
    m("ipv6.ignore-auto-dns", "no");
    let _ = r("nmcli", &["connection", "up", conn], None, true);
    let _ = std::fs::remove_file(dns_state());
    eprintln!("xdj: dns reverted on \"{conn}\"");
}

struct C {
    sb: u8,
    pb: u8,
    pk: String,
    ep: String,
    ul: String,
    xk: String,
    dns: String,
    domain: String,
    routes: Vec<String>,
    port: u16,
    wmtu: u16,
    ka: u16,
}

/// Where macOS's wireguard-go keeps its sockets and name files.
const MAC_RUN: &str = "/var/run/wireguard";

impl C {
    fn load() -> C {
        let m = conf();
        let g = |k: &str, d: &str| m.get(k).cloned().unwrap_or_else(|| d.into());
        C {
            sb: num(g("self_box", "1")),
            pb: num(g("peer_box", "0")),
            pk: g("peer_pubkey", ""),
            ep: g("peer_endpoint", ""),
            ul: g("underlay", "wg-flock"),
            xk: g("xdebbie_key", ""),
            // The mesh resolver, asked only for `dns_domain` (scoped DNS).
            // Empty = leave DNS alone.
            dns: g("dns", "10.23.0.11"),
            domain: g("dns_domain", ""),
            // Routed into the tunnel besides the underlay. The hub forwards
            // them to its VMs (xdebbied `[network] mesh_routed = true`).
            routes: g("routes", "10.23.0.0/16")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            port: num(g("wg_port", "51820")),
            wmtu: num(g("wg_mtu", "1420")),
            ka: num(g("keepalive", "25")),
        }
    }

    fn me(&self) -> String {
        format!("10.22.{}.1", self.sb)
    }
    fn peer(&self) -> String {
        format!("10.22.{}.1", self.pb)
    }

    fn link(&self) -> os::Link {
        os::Link {
            name: self.ul.clone(),
            me: self.me(),
            hub: self.peer(),
            hub_pubkey: if self.pk.is_empty() {
                "<hub_pubkey>".into()
            } else {
                self.pk.clone()
            },
            endpoint: ep_with_port(&self.ep, self.port),
            port: self.port,
            mtu: self.wmtu,
            keepalive: self.ka,
            routes: self.routes.clone(),
            dns: self.dns.split('/').next().unwrap_or("").to_string(),
            domain: self.domain.trim_matches('.').to_string(),
            key_file: kpath(),
        }
    }

    /// Our private key, and a word on where it came from.
    ///   local    already saved beside the conf
    ///   adopted  this box's xdebbie key — the hub knows this pubkey
    ///   pasted   you handed one over
    ///   minted   freshly generated — register it on the hub
    /// Adopting / prompting needs root (the xdebbie key is root-only), so
    /// callers short of a local key must be root (the re-exec sees to it).
    fn identity(&self) -> Result<(String, &'static str)> {
        let kp = kpath();
        if let Ok(s) = std::fs::read_to_string(&kp) {
            if !s.trim().is_empty() {
                return Ok((s.trim().into(), "local"));
            }
        }
        // Adopt this box's xdebbie underlay key if we can see it.
        let cands = [
            self.xk.clone(),
            env("XDJ_XDEBBIE_KEY").unwrap_or_default(),
            "/var/lib/xdebbie/wg.key".into(),
        ];
        for cand in cands {
            if cand.is_empty() {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(&cand) {
                let k = s.trim();
                if !k.is_empty() && pubkey_of(k).is_ok() {
                    save(&kp, &format!("{k}\n"), "600")?;
                    return Ok((k.into(), "adopted"));
                }
            }
        }
        // Otherwise: paste one, or Enter to mint.
        let typed = ask("WireGuard private key to use (Enter to generate one): ");
        let (k, tag) = if typed.is_empty() {
            (
                r(&os::Os::here().wg(), &["genkey"], None, false)?
                    .trim()
                    .to_string(),
                "minted",
            )
        } else {
            (typed, "pasted")
        };
        pubkey_of(&k).map_err(|_| e("that is not a valid WireGuard private key"))?;
        save(&kp, &format!("{k}\n"), "600")?;
        Ok((k, tag))
    }

    /// Fill the hub's pubkey/endpoint, asking once and remembering the answers.
    fn ensure_peer(&mut self) -> Result<()> {
        if self.pk.is_empty() {
            loop {
                let v = ask("Hub (master) WireGuard public key [`xdctl mesh wg` on the hub]: ");
                if !v.is_empty() {
                    self.pk = v;
                    break;
                }
                eprintln!("  — required; paste the hub's public key.");
            }
            conf_set("peer_pubkey", &self.pk)?;
        }
        if self.ep.is_empty() {
            // A remote box is usually driven over ssh from behind the hub, so
            // the address `w` shows us logged in from is offered as the default.
            self.ep = match guess_endpoint() {
                Some(g) => match ask(&format!(
                    "Hub endpoint host:port (Enter for {g}, seen in `w`; - for none / NAT'd hub): "
                ))
                .as_str()
                {
                    "" => g,
                    "-" => String::new(),
                    v => v.to_string(),
                },
                None => ask("Hub endpoint host:port (Enter for none / NAT'd hub): "),
            };
            if !self.ep.is_empty() {
                conf_set("peer_endpoint", &self.ep)?;
            }
        }
        Ok(())
    }

    /// Is our link up? (read-only)
    fn is_up(&self) -> bool {
        match os::Os::here() {
            os::Os::Linux => r("ip", &["link", "show", &self.ul], None, false).is_ok(),
            os::Os::Mac => mac_utun(&self.ul)
                .map(|u| r("ifconfig", &[&u], None, false).is_ok())
                .unwrap_or(false),
            os::Os::Win => r("sc", &["query", &os::Os::win_service(&self.ul)], None, true)
                .map(|o| o.contains("RUNNING"))
                .unwrap_or(false),
        }
    }

    fn up(&mut self) -> Result<()> {
        let here = os::Os::here();
        if let Some(m) = here.missing_wireguard(on_path, |p| std::path::Path::new(p).exists()) {
            return Err(e(m));
        }
        let (priv_, tag) = self.identity()?;
        self.ensure_peer()?;
        let l = self.link();
        match here {
            os::Os::Linux => {
                // An xdj from before routing left a Geneve tunnel and system-wide
                // DNS behind; clear those first.
                legacy_cleanup(self.pb);
                run(&os::Os::linux_up(&l))?;
            }
            os::Os::Mac => {
                let (c, name_file) = os::Os::mac_create(&l, MAC_RUN);
                run(&c)?;
                let utun = std::fs::read_to_string(&name_file)
                    .map(|s| s.trim().to_string())
                    .map_err(|x| {
                        e(format!(
                            "wireguard-go wrote no interface name to {name_file}: {x}"
                        ))
                    })?;
                run(&os::Os::mac_up(&l, &utun))?;
            }
            os::Os::Win => {
                let conf = win_conf_path(&self.ul);
                save(&conf, &os::Os::win_conf(&l, &priv_), "600")?;
                let cmds = os::Os::win_up(&l, &conf);
                run(&cmds[..2])?; // icacls, install the tunnel service
                wait_for_adapter(&self.ul);
                run(&cmds[2..])?; // routes
            }
        }
        self.dns_up(&l);

        let pk = pubkey_of(&priv_).unwrap_or_default();
        eprintln!(
            "xdj: up — {} on {}, peer {}, routing {}",
            self.me(),
            self.ul,
            self.peer(),
            self.routes.join(", ")
        );
        if tag == "adopted" {
            eprintln!("xdj: adopted this box's xdebbie key — the hub already knows it");
        } else {
            // minted / pasted / local: the hub may not have this box yet.
            eprintln!("xdj: key {pk}");
            eprintln!("xdj: if the hub does not know this box yet, run there:");
            eprintln!("      xdctl mesh peer add {} --pubkey {pk}", self.sb);
        }
        eprintln!("xdj: the hub reaches its VMs for you only with `[network] mesh_routed = true`");
        Ok(())
    }

    /// Scoped DNS: only `dns_domain` goes to the mesh resolver. Failure here
    /// never takes the link down.
    fn dns_up(&self, l: &os::Link) {
        if l.dns.is_empty() {
            return;
        }
        if l.domain.is_empty() {
            eprintln!("xdj: set dns_domain = <the mesh zone> in {} to resolve it through {}; DNS left alone", cpath(), l.dns);
            return;
        }
        let done = match os::Os::here() {
            os::Os::Linux if on_path("resolvectl") => run(&os::Os::linux_dns(l)),
            os::Os::Linux => Err(e(
                "scoped DNS needs systemd-resolved (resolvectl); DNS left alone",
            )),
            os::Os::Mac => {
                let (p, body) = os::Os::mac_dns(l);
                if std::path::Path::new(&p).exists() && !os::ours(&p) {
                    Err(e(format!("{p} exists and is not xdj's; DNS left alone")))
                } else {
                    std::fs::create_dir_all("/etc/resolver")
                        .and_then(|_| std::fs::write(&p, body))
                        .map_err(|x| e(format!("{p}: {x}")))
                }
            }
            os::Os::Win => run(&[os::Os::win_dns(l)]),
        };
        match done {
            Ok(()) => {
                eprintln!("xdj: dns — {} via {}", l.domain, l.dns);
                if !dns_answers(&l.dns) {
                    eprintln!("xdj: {} does not answer yet (is the hub routing for us: mesh_routed, and this box registered?)", l.dns);
                }
            }
            Err(x) => eprintln!("xdj: {x}"),
        }
    }

    fn down(&self) -> Result<()> {
        let l = self.link();
        match os::Os::here() {
            os::Os::Linux => {
                legacy_cleanup(self.pb);
                run(&os::Os::linux_down(&l))?;
            }
            os::Os::Mac => {
                let (p, _) = os::Os::mac_dns(&l);
                if !l.domain.is_empty() && os::ours(&p) {
                    let _ = std::fs::remove_file(&p);
                }
                if let Some(u) = mac_utun(&self.ul) {
                    run(&os::Os::mac_down(&u, MAC_RUN))?;
                }
                let _ = std::fs::remove_file(format!("{MAC_RUN}/{}.name", self.ul));
            }
            os::Os::Win => {
                run(&os::Os::win_down(&l))?;
                let _ = std::fs::remove_file(win_conf_path(&self.ul));
            }
        }
        eprintln!("xdj: down — left the mesh");
        Ok(())
    }

    /// Forget both keys: our saved private key and the hub's public key. The
    /// endpoint, box numbers and the rest of the conf stay. Links that are up
    /// keep the keys they were given until they are taken down.
    fn clear(&self) -> Result<()> {
        let kp = kpath();
        match std::fs::remove_file(&kp) {
            Ok(()) => eprintln!("xdj: removed {kp}"),
            Err(x) if x.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("xdj: no saved key at {kp}")
            }
            Err(x) => return Err(e(format!("{kp}: {x}"))),
        }
        if conf_del("peer_pubkey")? {
            eprintln!("xdj: forgot the hub's public key ({})", cpath());
        } else {
            eprintln!("xdj: no hub public key in {}", cpath());
        }
        if self.is_up() {
            eprintln!(
                "xdj: {} is still up on the old keys — `xdj down`, then `xdj up`",
                self.ul
            );
        } else {
            eprintln!("xdj: `xdj up` will adopt or ask for a key, and ask for the hub's");
        }
        Ok(())
    }

    fn status(&self) {
        println!(
            "link {} {}",
            self.ul,
            if self.is_up() { "up" } else { "down" }
        );
    }

    /// The moves `up` would make here, made of none.
    fn show(&self) {
        let l = self.link();
        let p = |v: Vec<os::Cmd>| v.iter().for_each(|c| println!("{c}"));
        match os::Os::here() {
            os::Os::Linux => {
                p(os::Os::linux_up(&l));
                if !l.dns.is_empty() && !l.domain.is_empty() {
                    p(os::Os::linux_dns(&l));
                }
            }
            os::Os::Mac => {
                p(os::Os::mac_create(&l, MAC_RUN).0);
                p(os::Os::mac_up(&l, "<utun>"));
                if !l.dns.is_empty() && !l.domain.is_empty() {
                    let (f, body) = os::Os::mac_dns(&l);
                    println!("# {f}:\n{body}");
                }
            }
            os::Os::Win => {
                println!(
                    "# {}:\n{}",
                    win_conf_path(&self.ul),
                    os::Os::win_conf(&l, "<private key>")
                );
                p(os::Os::win_up(&l, &win_conf_path(&self.ul)));
                if !l.dns.is_empty() && !l.domain.is_empty() {
                    println!("{}", os::Os::win_dns(&l));
                }
            }
        }
    }
}

/// Run a plan in order; the first hard failure stops it.
fn run(cmds: &[os::Cmd]) -> Result<()> {
    for c in cmds {
        let a: Vec<&str> = c.a.iter().map(String::as_str).collect();
        r(&c.p, &a, None, c.soft)?;
    }
    Ok(())
}

/// Is `prog` on PATH? (no `which`: Windows has none)
fn on_path(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|ps| {
            std::env::split_paths(&ps)
                .any(|d| d.join(prog).is_file() || d.join(format!("{prog}.exe")).is_file())
        })
        .unwrap_or(false)
}

/// macOS: the utun wireguard-go made for `name`, if it is still there.
fn mac_utun(name: &str) -> Option<String> {
    let u = std::fs::read_to_string(format!("{MAC_RUN}/{name}.name"))
        .ok()?
        .trim()
        .to_string();
    (!u.is_empty()).then_some(u)
}

/// Windows: the tunnel service's config, outside any one user's profile.
fn win_conf_path(name: &str) -> String {
    format!(
        r"{}\xdj\{name}.conf",
        env("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into())
    )
}

/// Windows: the tunnel service makes the adapter a moment after it starts;
/// routes need it to exist.
fn wait_for_adapter(name: &str) {
    for _ in 0..20 {
        if r(
            "netsh",
            &["interface", "ipv4", "show", "interfaces"],
            None,
            true,
        )
        .map(|o| o.contains(name))
        .unwrap_or(false)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    eprintln!("xdj: adapter {name} did not show up in 10 s; adding routes anyway");
}

/// Linux: what an xdj from before routing left — a Geneve tunnel and the
/// uplink's DNS pointed system-wide at the mesh. Harmless when absent.
fn legacy_cleanup(pb: u8) {
    let _ = r("ip", &["link", "del", &format!("gnv-b{pb}")], None, true);
    dns_revert();
}

/// Re-run ourselves under sudo, handing over $HOME and the user name so the
/// root half reads your conf and gives any files it writes back to you.
fn reexec(mode: &str) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let (h, u) = (env("HOME").unwrap_or_default(), whoami());
    let mut x = X::new("sudo");
    x.arg("env")
        .arg("XDJ_ROOT=1")
        .arg(format!("XDJ_HOME={h}"))
        .arg(format!("XDJ_USER={u}"));
    x.arg(exe).arg(mode);
    Ok(x.status()?.code().unwrap_or(1))
}

fn whoami() -> String {
    env("USER").or_else(|| env("LOGNAME")).unwrap_or_else(|| {
        r("id", &["-un"], None, true)
            .map(|s| s.trim().into())
            .unwrap_or_default()
    })
}

fn main() {
    std::process::exit(match go() {
        Ok(n) => n,
        Err(x) => {
            eprintln!("xdj: {x}");
            1
        }
    });
}

fn go() -> Result<i32> {
    let a = std::env::args().nth(1).unwrap_or_default();
    // No verb: toggle on the underlay's presence.
    let mode = if a.is_empty() {
        if C::load().is_up() {
            "down".into()
        } else {
            "up".into()
        }
    } else {
        a
    };

    if !matches!(mode.as_str(), "-h" | "--help" | "help") && os::Os::here() == os::Os::Linux {
        if let Some(pid) = daemon_pid("/proc") {
            return Err(e(format!("xdebbied is running here (pid {pid}) and owns the mesh on this box — use xdctl mesh, or stop the daemon first")));
        }
    }

    match mode.as_str() {
        "show" => {
            C::load().show();
            Ok(0)
        }
        "status" => {
            C::load().status();
            Ok(0)
        }
        "key" => {
            // Print from a local key without root; else let root resolve it.
            if std::fs::read_to_string(kpath())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
                || rooted()
            {
                println!("{}", pubkey_of(&C::load().identity()?.0)?);
                Ok(0)
            } else {
                reexec("key")
            }
        }
        "clear" => {
            C::load().clear()?;
            Ok(0)
        }
        "up" | "down" => {
            if os::Os::here() == os::Os::Win {
                // No sudo to lean on: `net session` succeeds only elevated.
                if r("net", &["session"], None, false).is_err() {
                    return Err(e("run xdj from an elevated prompt (Run as administrator)"));
                }
            } else if !rooted() {
                return reexec(&mode);
            }
            let mut c = C::load();
            if mode == "up" {
                c.up()?
            } else {
                c.down()?
            }
            Ok(0)
        }
        "-h" | "--help" | "help" => {
            eprintln!("usage: xdj [up|down|key|clear|show|status]   (no arg = toggle; conf ~/.config/xdj/xdj.conf)");
            Ok(0)
        }
        o => Err(e(format!(
            "unknown move {o:?}; try up|down|key|clear|show|status"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_del_drops_only_that_key() {
        let p = std::env::temp_dir().join(format!("xdj-test-{}.conf", std::process::id()));
        std::env::set_var("XDJ_CONF", &p);
        std::fs::write(
            &p,
            "self_box = 3\npeer_pubkey = abc=  # hub\n# note\npeer_endpoint = h:51820\n",
        )
        .unwrap();
        assert!(conf_del("peer_pubkey").unwrap());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "self_box = 3\n# note\npeer_endpoint = h:51820\n"
        );
        assert!(!conf_del("peer_pubkey").unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn daemon_is_found_by_comm() {
        let d = std::env::temp_dir().join(format!("xdj-proc-{}", std::process::id()));
        for (pid, comm) in [
            ("41", "sshd"),
            ("self", "xdebbied"),
            ("77", "xdebbied-helper"),
        ] {
            std::fs::create_dir_all(d.join(pid)).unwrap();
            std::fs::write(d.join(pid).join("comm"), format!("{comm}\n")).unwrap();
        }
        assert_eq!(daemon_pid(&d.to_string_lossy()), None);
        std::fs::create_dir_all(d.join("1807")).unwrap();
        std::fs::write(d.join("1807/comm"), "xdebbied\n").unwrap();
        assert_eq!(daemon_pid(&d.to_string_lossy()), Some(1807));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn w_guess_takes_the_ssh_source() {
        // A session without a tty has no TTY column: FROM is the second field.
        let w = "root              203.0.113.9     18:44           0.00s   ?    sshd-session: root [priv]\n\
                 root     pts/0    203.0.113.9     18:25   17:55   0.01s  0.01s -bash\n";
        assert_eq!(from_w(w).as_deref(), Some("203.0.113.9"));
    }

    #[test]
    fn w_guess_prefers_public_and_ignores_the_command_column() {
        let w = "me   pts/0  192.168.1.20   10:00  1:00  0.01s 0.01s -bash\n\
                 me   pts/1  192.168.1.20   10:01  1:00  0.01s 0.01s -bash\n\
                 me   pts/2  198.51.100.7   10:02  1:00  0.01s 0.01s ssh 10.9.9.9\n";
        assert_eq!(from_w(w).as_deref(), Some("198.51.100.7"));
        // Local sessions only: nothing to go on.
        let local = "me  tty2  -      12:19  1:24m 0.01s 0.01s gnome-session ping 10.9.9.9\n\
                     me  pts/3 :0     12:20  1:00  0.01s 0.01s -bash\n\
                     me  pts/4 127.0.0.1 12:21 1:00 0.01s 0.01s -bash\n";
        assert_eq!(from_w(local), None);
    }
}
