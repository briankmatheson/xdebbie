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

/// The 17 slot MACs a `derived` box owns: 52:54:00:23:<box>:{00..0f,fe}.
fn macs(b: u8) -> Vec<String> {
    let mut v: Vec<String> = (0u8..16).map(|s| format!("52:54:00:23:{b:02x}:{s:02x}")).collect();
    v.push(format!("52:54:00:23:{b:02x}:fe"));
    v
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

struct C {
    sb: u8, pb: u8, pk: String, ep: String,
    br: String, ul: String, xk: String,
    port: u16, vni: u32, dport: u16, wmtu: u16, mmtu: u16, ka: u16,
}

impl C {
    fn load() -> C {
        let m = conf();
        let g = |k: &str, d: &str| m.get(k).cloned().unwrap_or_else(|| d.into());
        C {
            sb: num(g("self_box", "1")),
            pb: num(g("peer_box", "0")),
            pk: g("peer_pubkey", ""),
            ep: g("peer_endpoint", ""),
            br: g("bridge", "virbr-xdebbie"),
            ul: g("underlay", "wg-flock"),
            xk: g("xdebbie_key", ""),
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
        r("bridge", &["link", "set", "dev", &d, "isolated", "on", "learning", "off", "flood",
            "off", "mcast_flood", "on", "bcast_flood", "on", "neigh_suppress", "off"], None, false)?;
        r("ip", &["link", "set", &d, "up"], None, false)?;
        for m in macs(self.pb) { r("bridge", &["fdb", "replace", &m, "dev", &d, "master", "static"], None, false)?; }

        let pk = pubkey_of(&priv_).unwrap_or_default();
        eprintln!("xdj: up — {} on {ul}, peer {}", self.me(), self.peer());
        match tag {
            "minted" => eprintln!("xdj: minted a new key — register it on the hub:\n      {pk}"),
            "pasted" => eprintln!("xdj: using the key you gave (pubkey {pk})"),
            "adopted" => eprintln!("xdj: adopted this box's xdebbie key — the hub already knows it"),
            _ => eprintln!("xdj: key {pk}"),
        }
        Ok(())
    }

    fn down(&self) -> Result<()> {
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
        println!("bridge link set dev {d} isolated on learning off flood off mcast_flood on bcast_flood on neigh_suppress off");
        println!("bridge fdb replace {{{} macs}} dev {d} master static", macs(self.pb).len());
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
