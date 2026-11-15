// SPDX-License-Identifier: GPL-3.0-or-later
//! xdj — slip into the xdebbie mesh without running the daemon.
//!
//! It brings up the two links xdebbied would: a WireGuard underlay
//! (wg-flock, 10.22.<me>.1/16) with one peer per hub, and a Geneve tunnel
//! (gnv-b<peer>) folded into the local bridge with the derived static FDB.
//!
//!   xdj           toggle: up if the tunnel is down, down if it is up
//!   xdj up/down   force a direction
//!   xdj key       print our public key (register it on the hub)
//!   xdj show      print the moves, make none
//!   xdj status    say whether the links are up
//!
//! Identity: it adopts this box's xdebbie WireGuard key if one is on disk
//! (/var/lib/xdebbie/wg.key), so the hub already knows us; otherwise it asks
//! for a key to paste (Enter mints a fresh one) and saves it beside the conf.
//! The hub's public key and endpoint are asked for once and kept in
//! ~/.config/xdj/xdj.conf. The ip/wg/bridge calls want root — it re-execs
//! under sudo, carrying your $HOME so it reads your conf, not root's.

use std::collections::BTreeMap as M;
use std::io::{Error, ErrorKind::Other, Result, Write};
use std::process::{Command as X, Stdio as Z};

const A: &str = "xdebbie-mesh/v1"; // the ownership alias the daemon stamps

fn num<T: std::str::FromStr + Default>(s: String) -> T { s.trim().parse().unwrap_or_default() }
fn e(m: impl Into<String>) -> Error { Error::new(Other, m.into()) }
fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|s| !s.is_empty()) }
fn rooted() -> bool { env("XDJ_ROOT").is_some() }
// $HOME, but under sudo the user half hands theirs over so root reads it too.
fn home() -> String { env("XDJ_HOME").or_else(|| env("HOME")).unwrap_or_else(|| "/root".into()) }
fn cpath() -> String { env("XDJ_CONF").unwrap_or_else(|| format!("{}/.config/xdj/xdj.conf", home())) }
fn kpath() -> String { format!("{}/.config/xdj/wg.key", home()) }

/// Run `p a…`, feeding `i` on stdin; `soft` swallows a failure (link exists).
fn r(p: &str, a: &[&str], i: Option<&str>, soft: bool) -> Result<String> {
    let mut c = X::new(p);
    c.args(a).stdin(if i.is_some() { Z::piped() } else { Z::null() }).stdout(Z::piped()).stderr(Z::piped());
    let mut ch = c.spawn().map_err(|x| e(format!("{p}: {x}")))?;
    if let (Some(s), Some(mut h)) = (i, ch.stdin.take()) { let _ = h.write_all(s.as_bytes()); }
    let o = ch.wait_with_output()?;
    if o.status.success() || soft { return Ok(String::from_utf8_lossy(&o.stdout).into()); }
    Err(e(format!("{p} {}: {}", a.join(" "), String::from_utf8_lossy(&o.stderr).trim())))
}

/// Does a link exist? (read-only, no root needed).
fn have(dev: &str) -> bool { r("ip", &["link", "show", dev], None, false).is_ok() }

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
        if let Some(u) = env("XDJ_USER") { let _ = r("chown", &[&format!("{u}:"), path], None, true); }
    }
}

fn save(path: &str, body: &str, mode: &str) -> Result<()> {
    if let Some(d) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(d)?;
        mine(&d.to_string_lossy());
    }
    std::fs::write(path, body)?;
    let _ = r("chmod", &[mode, path], None, true);
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
        if bare.split_once('=').map(|(k, _)| k.trim() == key).unwrap_or(false) {
            out.push_str(&format!("{key} = {val}\n"));
            hit = true;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    if !hit { out.push_str(&format!("{key} = {val}\n")); }
    save(&p, &out, "644")
}

fn pubkey_of(priv_: &str) -> Result<String> {
    Ok(r("wg", &["pubkey"], Some(&format!("{}\n", priv_.trim())), false)?.trim().into())
}

fn dns_state() -> String { format!("{}/.config/xdj/dns.conn", home()) }

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

/// The interface carrying the default route (gram's real uplink).
fn default_iface() -> Option<String> {
    let o = r("ip", &["route", "show", "default"], None, true).ok()?;
    let t: Vec<&str> = o.lines().next().unwrap_or("").split_whitespace().collect();
    t.iter().position(|&w| w == "dev").and_then(|i| t.get(i + 1)).map(|s| s.to_string())
}

/// The active NetworkManager connection on `iface`.
fn conn_for(iface: &str) -> Option<String> {
    let o = r("nmcli", &["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"], None, true).ok()?;
    o.lines().find_map(|l| l.rsplit_once(':').filter(|(_, d)| *d == iface).map(|(n, _)| n.to_string()))
}

/// Point the uplink connection at the internal resolver and remember which
/// one we changed, so `down` can put it back. Mirrors simple/bin/set-dns.sh.
fn dns_set(server: &str) -> Result<()> {
    let Some(iface) = default_iface() else { eprintln!("xdj: no default route; leaving DNS alone"); return Ok(()); };
    let Some(conn) = conn_for(&iface) else { eprintln!("xdj: no NM connection on {iface}; leaving DNS alone"); return Ok(()); };
    let m = |k: &str, v: &str, soft: bool| r("nmcli", &["connection", "modify", &conn, k, v], None, soft).map(|_| ());
    m("ipv4.dns", "", false)?;
    m("ipv4.dns", server, false)?;
    m("ipv6.dns", "", true)?;
    m("ipv4.ignore-auto-dns", "yes", false)?;
    m("ipv6.ignore-auto-dns", "yes", true)?;
    r("nmcli", &["connection", "up", &conn], None, false)?;
    save(&dns_state(), &format!("{conn}\n"), "644")?;
    eprintln!("xdj: dns -> {server} on \"{conn}\"");
    Ok(())
}

/// Undo `dns_set`: hand the remembered connection its DHCP resolvers back.
fn dns_revert() {
    let Ok(c) = std::fs::read_to_string(dns_state()) else { return; };
    let conn = c.trim();
    if conn.is_empty() { return; }
    let m = |k: &str, v: &str| { let _ = r("nmcli", &["connection", "modify", conn, k, v], None, true); };
    m("ipv4.dns", "");
    m("ipv4.ignore-auto-dns", "no");
    m("ipv6.ignore-auto-dns", "no");
    let _ = r("nmcli", &["connection", "up", conn], None, true);
    let _ = std::fs::remove_file(dns_state());
    eprintln!("xdj: dns reverted on \"{conn}\"");
}

struct C {
    sb: u8, pb: u8, pk: String, ep: String,
    br: String, ul: String, xk: String, oaddr: String, dns: String,
    port: u16, vni: u32, dport: u16, wmtu: u16, mmtu: u16, ka: u16,
}

impl C {
    fn load() -> C {
        let m = conf();
        let g = |k: &str, d: &str| m.get(k).cloned().unwrap_or_else(|| d.into());
        let sb: u8 = num(g("self_box", "1"));
        C {
            sb,
            pb: num(g("peer_box", "0")),
            pk: g("peer_pubkey", ""),
            ep: g("peer_endpoint", ""),
            br: g("bridge", "virbr-xdebbie"),
            ul: g("underlay", "wg-flock"),
            xk: g("xdebbie_key", ""),
            // Our address on the overlay bridge, so we can reach 10.23.x
            // services (and the internal DNS). Empty = don't assign one.
            oaddr: g("overlay", &format!("10.23.{sb}.1/16")),
            // Internal resolver, set while the tunnel is up. Empty = leave
            // the system DNS alone.
            dns: g("dns", "10.23.0.11"),
            port: num(g("wg_port", "51820")),
            vni: num(g("vni", "23")),
            dport: num(g("dstport", "6081")),
            wmtu: num(g("wg_mtu", "1420")),
            mmtu: num(g("mesh_mtu", "1370")),
            ka: num(g("keepalive", "25")),
        }
    }

    fn me(&self) -> String { format!("10.22.{}.1", self.sb) }
    fn peer(&self) -> String { format!("10.22.{}.1", self.pb) }
    fn tun(&self) -> String { format!("gnv-b{}", self.pb) }

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
            if !s.trim().is_empty() { return Ok((s.trim().into(), "local")); }
        }
        // Adopt this box's xdebbie underlay key if we can see it.
        let cands = [self.xk.clone(), env("XDJ_XDEBBIE_KEY").unwrap_or_default(), "/var/lib/xdebbie/wg.key".into()];
        for cand in cands {
            if cand.is_empty() { continue; }
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
            (r("wg", &["genkey"], None, false)?.trim().to_string(), "minted")
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
                let v = ask("Hub (master) WireGuard public key [`xdj key` on the hub]: ");
                if !v.is_empty() { self.pk = v; break; }
                eprintln!("  — required; paste the hub's public key.");
            }
            conf_set("peer_pubkey", &self.pk)?;
        }
        if self.ep.is_empty() {
            self.ep = ask("Hub endpoint host:port (Enter for none / NAT'd hub): ");
            if !self.ep.is_empty() { conf_set("peer_endpoint", &self.ep)?; }
        }
        Ok(())
    }

    fn keyiface(&self, priv_: &str) -> Result<()> {
        let s = format!("{}\n", priv_.trim());
        let p = self.port.to_string();
        if r("wg", &["set", &self.ul, "private-key", "/dev/stdin", "listen-port", &p], Some(&s), false).is_err() {
            r("wg", &["set", &self.ul, "private-key", &kpath(), "listen-port", &p], None, false)?;
        }
        Ok(())
    }

    fn up(&mut self) -> Result<()> {
        let (priv_, tag) = self.identity()?;
        self.ensure_peer()?;
        let (ul, wmtu) = (self.ul.clone(), self.wmtu.to_string());
        r("ip", &["link", "add", &ul, "type", "wireguard"], None, true)?;
        r("ip", &["link", "set", &ul, "alias", A], None, false)?;
        self.keyiface(&priv_)?;
        r("ip", &["addr", "replace", &format!("{}/16", self.me()), "dev", &ul], None, false)?;
        r("ip", &["link", "set", &ul, "mtu", &wmtu], None, false)?;
        r("ip", &["link", "set", &ul, "up"], None, false)?;
        let rem = self.peer();
        let ka = self.ka.to_string();
        let aip = format!("{rem}/32");
        let mut pa = vec!["set", &ul, "peer", &self.pk];
        if !self.ep.is_empty() { pa.extend(["endpoint", &self.ep]); }
        pa.extend(["allowed-ips", &aip, "persistent-keepalive", &ka]);
        r("wg", &pa, None, false)?;

        let (d, vni, mmtu, dp) = (self.tun(), self.vni.to_string(), self.mmtu.to_string(), self.dport.to_string());
        r("ip", &["link", "add", &d, "type", "geneve", "id", &vni, "remote", &rem, "dstport", &dp, "ttl", "64", "df", "set"], None, true)?;
        r("ip", &["link", "set", &d, "alias", A], None, false)?;
        r("ip", &["link", "set", &d, "mtu", &mmtu], None, false)?;
        r("ip", &["link", "set", &d, "master", &self.br], None, false)?;
        // Learn mode: flood BUM to the single tunnel and learn MACs from it,
        // so we reach the hub's bridge whatever its MAC is. The derived-MAC
        // static FDB only worked when every bridge carried a 52:54:00:23:*
        // MAC; the hub's does not, so unicast to it was dropped.
        r("bridge", &["link", "set", "dev", &d, "isolated", "on", "learning", "on", "flood",
            "on", "mcast_flood", "on", "bcast_flood", "on", "neigh_suppress", "off"], None, false)?;
        r("ip", &["link", "set", &d, "up"], None, false)?;

        // Our place on the overlay, then the internal resolver on top of it.
        if !self.oaddr.is_empty() { r("ip", &["addr", "replace", &self.oaddr, "dev", &self.br], None, false)?; }
        if !self.dns.is_empty() {
            // Only hand the resolver over once it actually answers over the
            // mesh — otherwise a half-up tunnel would cost you all DNS.
            let host = self.dns.split('/').next().unwrap_or(&self.dns);
            if dns_answers(host) {
                if let Err(e) = dns_set(host) { eprintln!("xdj: dns setup failed ({e}); mesh is still up"); }
            } else {
                eprintln!("xdj: {host} unreachable over the mesh — leaving DNS alone");
                eprintln!("      (is this box registered on the hub, and the tunnel passing traffic?)");
            }
        }

        let pk = pubkey_of(&priv_).unwrap_or_default();
        eprintln!("xdj: up — {} on {ul}, peer {}", self.me(), self.peer());
        if tag == "adopted" {
            eprintln!("xdj: adopted this box's xdebbie key — the hub already knows it");
        } else {
            // minted / pasted / local: the hub may not have this box yet.
            eprintln!("xdj: key {pk}");
            eprintln!("xdj: if the hub does not know this box yet, run there:");
            eprintln!("      xdctl mesh peer add {} --pubkey {pk}", self.sb);
        }
        Ok(())
    }

    fn down(&self) -> Result<()> {
        dns_revert();
        if !self.oaddr.is_empty() { r("ip", &["addr", "del", &self.oaddr, "dev", &self.br], None, true)?; }
        r("ip", &["link", "del", &self.tun()], None, true)?;
        r("ip", &["link", "del", &self.ul], None, true)?;
        eprintln!("xdj: down — left the mesh");
        Ok(())
    }

    fn status(&self) {
        let s = |d: &str| if have(d) { "up" } else { "down" };
        println!("underlay {} {}\ntunnel   {} {}", self.ul, s(&self.ul), self.tun(), s(&self.tun()));
    }

    fn show(&self) {
        let pk = if self.pk.is_empty() { "<hub_pubkey>" } else { &self.pk };
        let (m, p) = (self.me(), self.peer());
        let ep = if self.ep.is_empty() { String::new() } else { format!("endpoint {} ", self.ep) };
        println!("ip link add {0} type wireguard; ip link set {0} alias {A}", self.ul);
        println!("wg set {} private-key {} listen-port {}", self.ul, kpath(), self.port);
        println!("ip addr replace {m}/16 dev {0}; ip link set {0} mtu {1} up", self.ul, self.wmtu);
        println!("wg set {} peer {pk} {ep}allowed-ips {p}/32 persistent-keepalive {}", self.ul, self.ka);
        let d = self.tun();
        println!("ip link add {d} type geneve id {} remote {p} dstport {} ttl 64 df set", self.vni, self.dport);
        println!("ip link set {d} alias {A} mtu {} master {} up", self.mmtu, self.br);
        println!("bridge link set dev {d} isolated on learning on flood on mcast_flood on bcast_flood on neigh_suppress off");
        if !self.oaddr.is_empty() { println!("ip addr replace {} dev {}", self.oaddr, self.br); }
        if !self.dns.is_empty() {
            println!("nmcli connection modify <uplink> ipv4.dns {} ipv4.ignore-auto-dns yes; nmcli connection up <uplink>", self.dns);
        }
    }
}

/// Re-run ourselves under sudo, handing over $HOME and the user name so the
/// root half reads your conf and gives any files it writes back to you.
fn reexec(mode: &str) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let (h, u) = (env("HOME").unwrap_or_default(), whoami());
    let mut x = X::new("sudo");
    x.arg("env").arg("XDJ_ROOT=1").arg(format!("XDJ_HOME={h}")).arg(format!("XDJ_USER={u}"));
    x.arg(exe).arg(mode);
    Ok(x.status()?.code().unwrap_or(1))
}

fn whoami() -> String {
    env("USER").or_else(|| env("LOGNAME"))
        .unwrap_or_else(|| r("id", &["-un"], None, true).map(|s| s.trim().into()).unwrap_or_default())
}

fn main() {
    std::process::exit(match go() {
        Ok(n) => n,
        Err(x) => { eprintln!("xdj: {x}"); 1 }
    });
}

fn go() -> Result<i32> {
    let a = std::env::args().nth(1).unwrap_or_default();
    // No verb: toggle on the underlay's presence.
    let mode = if a.is_empty() {
        if have(&C::load().ul) { "down".into() } else { "up".into() }
    } else { a };

    match mode.as_str() {
        "show" => { C::load().show(); Ok(0) }
        "status" => { C::load().status(); Ok(0) }
        "key" => {
            // Print from a local key without root; else let root resolve it.
            if std::fs::read_to_string(kpath()).map(|s| !s.trim().is_empty()).unwrap_or(false) || rooted() {
                println!("{}", pubkey_of(&C::load().identity()?.0)?);
                Ok(0)
            } else {
                reexec("key")
            }
        }
        "up" | "down" => {
            if !rooted() { return reexec(&mode); }
            let mut c = C::load();
            if mode == "up" { c.up()? } else { c.down()? }
            Ok(0)
        }
        "-h" | "--help" | "help" => {
            eprintln!("usage: xdj [up|down|key|show|status]   (no arg = toggle; conf ~/.config/xdj/xdj.conf)");
            Ok(0)
        }
        o => Err(e(format!("unknown move {o:?}; try up|down|key|show|status"))),
    }
}
