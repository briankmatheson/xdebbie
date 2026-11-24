// SPDX-License-Identifier: GPL-3.0-or-later
//! Reserved addresses from DNS: `[network] reserve_from_dns`.
//!
//! Every A record inside the overlay in the listed zones (fetched by zone
//! transfer, AXFR over TCP) is an address something already owns -- a
//! LoadBalancer, an API endpoint, a hub -- so the allocator never hands it
//! to a VM. DNS is the one list to keep: name the address and it is
//! reserved. A transfer that fails keeps that zone's last good set (also
//! saved to disk), so a DNS outage never drops a reservation.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{info, warn};

use super::model::Cidr;

/// One zone to read: `{ zone = "mesh.example.org", server = "10.23.0.1" }`.
/// `server` is an address (port 53 unless given), never a name: resolving
/// the DNS server through DNS would make the reservations depend on the
/// thing they protect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsSource {
    pub zone: String,
    pub server: String,
}

impl DnsSource {
    pub fn addr(&self) -> Result<SocketAddr> {
        if let Ok(a) = self.server.parse::<SocketAddr>() {
            return Ok(a);
        }
        let ip: std::net::IpAddr = self.server.parse().with_context(|| {
            format!(
                "reserve_from_dns: server {:?} is not an address",
                self.server
            )
        })?;
        Ok(SocketAddr::new(ip, 53))
    }
}

/// An address reserved because DNS names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsReserved {
    pub ip: Ipv4Addr,
    pub name: String,
    pub zone: String,
}

/// What `GET /v1/network` shows about it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DnsReservedStatus {
    pub entries: Vec<DnsReserved>,
    /// Per zone: when it last transferred (unix seconds), or why not.
    pub zones: BTreeMap<String, ZoneState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZoneState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub records: usize,
}

#[derive(Default)]
struct State {
    /// zone -> its A records inside the overlay (last good set)
    by_zone: BTreeMap<String, Vec<DnsReserved>>,
    zones: BTreeMap<String, ZoneState>,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(State::default()));

/// Address -> the name that reserves it, across every zone.
pub fn current() -> BTreeMap<Ipv4Addr, String> {
    let s = STATE.read().unwrap();
    let mut out = BTreeMap::new();
    for e in s.by_zone.values().flatten() {
        out.entry(e.ip).or_insert_with(|| e.name.clone());
    }
    out
}

pub fn status() -> DnsReservedStatus {
    let s = STATE.read().unwrap();
    let mut entries: Vec<DnsReserved> = s.by_zone.values().flatten().cloned().collect();
    entries.sort_by(|a, b| (a.ip, &a.name).cmp(&(b.ip, &b.name)));
    DnsReservedStatus {
        entries,
        zones: s.zones.clone(),
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    by_zone: BTreeMap<String, Vec<DnsReserved>>,
}

/// The last good sets from disk, before the first transfer: a box that
/// boots while DNS is down still keeps its reservations.
pub fn load(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    match serde_json::from_str::<Saved>(&text) {
        Ok(saved) => {
            let mut s = STATE.write().unwrap();
            for (zone, entries) in saved.by_zone {
                s.zones.entry(zone.clone()).or_default().records = entries.len();
                s.by_zone.insert(zone, entries);
            }
        }
        Err(e) => {
            warn!(path = %path.display(), "reserve_from_dns: ignoring unreadable saved set: {e}")
        }
    }
}

fn save(path: &Path) {
    let saved = Saved {
        by_zone: STATE.read().unwrap().by_zone.clone(),
    };
    let tmp = path.with_extension("json.tmp");
    let res = serde_json::to_vec_pretty(&saved)
        .map_err(anyhow::Error::from)
        .and_then(|b| std::fs::write(&tmp, b).map_err(Into::into))
        .and_then(|_| std::fs::rename(&tmp, path).map_err(Into::into));
    if let Err(e) = res {
        warn!(path = %path.display(), "reserve_from_dns: could not save: {e}");
    }
}

/// Transfer every zone once; a zone that fails keeps its last good set.
pub async fn refresh(sources: &[DnsSource], cidr: Cidr, saved: &Path) {
    let mut changed = false;
    for src in sources {
        let zone = src.zone.trim_end_matches('.').to_ascii_lowercase();
        let got = match src.addr() {
            Ok(a) => axfr(a, &zone, Duration::from_secs(10)).await,
            Err(e) => Err(e),
        };
        let now = chrono::Utc::now().timestamp();
        let mut s = STATE.write().unwrap();
        match got {
            Ok(records) => {
                let mut entries: Vec<DnsReserved> = records
                    .into_iter()
                    .filter(|(_, ip)| cidr.contains_host(*ip))
                    .map(|(name, ip)| DnsReserved {
                        ip,
                        name,
                        zone: zone.clone(),
                    })
                    .collect();
                entries.sort_by(|a, b| (a.ip, &a.name).cmp(&(b.ip, &b.name)));
                entries.dedup();
                let st = s.zones.entry(zone.clone()).or_default();
                st.last_ok = Some(now);
                st.error = None;
                st.records = entries.len();
                if s.by_zone.get(&zone) != Some(&entries) {
                    info!(%zone, records = entries.len(), "reserve_from_dns: reserved set changed");
                    s.by_zone.insert(zone, entries);
                    changed = true;
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let st = s.zones.entry(zone.clone()).or_default();
                if st.error.as_deref() != Some(msg.as_str()) {
                    warn!(%zone, server = %src.server, "reserve_from_dns: transfer failed, keeping the last good set: {msg}");
                }
                st.error = Some(msg);
            }
        }
    }
    // zones no longer configured stop reserving
    {
        let mut s = STATE.write().unwrap();
        let keep: Vec<String> = sources
            .iter()
            .map(|z| z.zone.trim_end_matches('.').to_ascii_lowercase())
            .collect();
        let before = s.by_zone.len();
        s.by_zone.retain(|z, _| keep.contains(z));
        s.zones.retain(|z, _| keep.contains(z));
        changed |= s.by_zone.len() != before;
    }
    if changed {
        save(saved);
    }
}

/// Where the last good sets are kept: beside the state file.
pub fn saved_path(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("dns-reserved.json")
}

/// Transfer now, then again every `every` (`load` first, at startup).
pub async fn run(sources: Vec<DnsSource>, cidr: Cidr, saved: PathBuf, every: Duration) {
    loop {
        refresh(&sources, cidr, &saved).await;
        tokio::time::sleep(every).await;
    }
}

// ---- AXFR ------------------------------------------------------------------

const T_A: u16 = 1;
const T_SOA: u16 = 6;
const T_AXFR: u16 = 252;
const C_IN: u16 = 1;
/// A zone this page-sized system keeps is small; refuse anything absurd.
const MAX_TOTAL: usize = 16 << 20;
const MAX_MESSAGES: usize = 4096;

/// The zone's A records as (name without the trailing dot, lowercase; ip).
pub async fn axfr(
    server: SocketAddr,
    zone: &str,
    timeout: Duration,
) -> Result<Vec<(String, Ipv4Addr)>> {
    tokio::time::timeout(timeout, axfr_inner(server, zone))
        .await
        .with_context(|| {
            format!("no complete transfer of {zone} from {server} within {timeout:?}")
        })?
}

async fn axfr_inner(server: SocketAddr, zone: &str) -> Result<Vec<(String, Ipv4Addr)>> {
    let id: u16 = rand::random();
    let q = query(id, zone)?;
    let mut sock = TcpStream::connect(server)
        .await
        .with_context(|| format!("connecting to {server}"))?;
    let mut framed = Vec::with_capacity(q.len() + 2);
    framed.extend_from_slice(&(q.len() as u16).to_be_bytes());
    framed.extend_from_slice(&q);
    sock.write_all(&framed).await?;
    let mut parser = Transfer::default();
    let mut total = 0usize;
    for _ in 0..MAX_MESSAGES {
        let len = sock
            .read_u16()
            .await
            .context("the server closed the transfer early")? as usize;
        total += len;
        if total > MAX_TOTAL {
            bail!("transfer larger than {MAX_TOTAL} bytes");
        }
        let mut msg = vec![0u8; len];
        sock.read_exact(&mut msg).await?;
        if parser.message(&msg, id)? {
            return Ok(parser.a);
        }
    }
    bail!("transfer did not end within {MAX_MESSAGES} messages")
}

fn query(id: u16, zone: &str) -> Result<Vec<u8>> {
    let mut m = Vec::with_capacity(64);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0, 0]); // flags: a standard query, no recursion
    m.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // 1 question
    for label in zone.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            bail!("{zone:?} is not a zone name");
        }
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&T_AXFR.to_be_bytes());
    m.extend_from_slice(&C_IN.to_be_bytes());
    Ok(m)
}

/// The answer stream: SOA, every record, SOA again.
#[derive(Default)]
struct Transfer {
    soas: usize,
    first: bool,
    a: Vec<(String, Ipv4Addr)>,
}

impl Transfer {
    /// Take one message; `true` once the closing SOA has been seen.
    fn message(&mut self, m: &[u8], id: u16) -> Result<bool> {
        if m.len() < 12 {
            bail!("short DNS message");
        }
        if u16::from_be_bytes([m[0], m[1]]) != id {
            bail!("answer for another query");
        }
        let rcode = m[3] & 0x0f;
        if rcode != 0 {
            bail!(
                "server answered {} (rcode {rcode}){}",
                match rcode {
                    1 => "FORMERR",
                    2 => "SERVFAIL",
                    3 => "NXDOMAIN",
                    4 => "NOTIMP",
                    5 => "REFUSED: allow zone transfers to this box",
                    9 => "NOTAUTH",
                    _ => "an error",
                },
                ""
            );
        }
        let qd = u16::from_be_bytes([m[4], m[5]]) as usize;
        let an = u16::from_be_bytes([m[6], m[7]]) as usize;
        let mut p = 12;
        for _ in 0..qd {
            let (_, np) = name(m, p)?;
            p = np + 4;
        }
        for _ in 0..an {
            let (owner, np) = name(m, p)?;
            p = np;
            if p + 10 > m.len() {
                bail!("truncated record");
            }
            let t = u16::from_be_bytes([m[p], m[p + 1]]);
            let class = u16::from_be_bytes([m[p + 2], m[p + 3]]);
            let rdlen = u16::from_be_bytes([m[p + 8], m[p + 9]]) as usize;
            p += 10;
            if p + rdlen > m.len() {
                bail!("truncated record data");
            }
            let rdata = &m[p..p + rdlen];
            p += rdlen;
            if !self.first {
                if t != T_SOA {
                    bail!("the transfer did not start with the zone's SOA");
                }
                self.first = true;
                self.soas = 1;
                continue;
            }
            if t == T_SOA {
                self.soas += 1;
                return Ok(true); // the closing SOA
            }
            if t == T_A && class == C_IN && rdlen == 4 {
                self.a
                    .push((owner, Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3])));
            }
        }
        Ok(false)
    }
}

/// A (possibly compressed) name at `p`: (dotted, lowercase, no trailing dot;
/// the offset just past it in the record).
fn name(m: &[u8], mut p: usize) -> Result<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    let mut total = 0usize;
    for _ in 0..128 {
        let Some(&len) = m.get(p) else {
            bail!("name runs past the message")
        };
        match len {
            0 => {
                let next = end.unwrap_or(p + 1);
                return Ok((labels.join("."), next));
            }
            l if l & 0xc0 == 0xc0 => {
                let Some(&lo) = m.get(p + 1) else {
                    bail!("truncated name pointer")
                };
                if end.is_none() {
                    end = Some(p + 2);
                }
                let target = (((l & 0x3f) as usize) << 8) | lo as usize;
                if target >= p {
                    bail!("name pointer does not point back");
                }
                p = target;
            }
            l if l & 0xc0 == 0 => {
                let l = l as usize;
                let Some(bytes) = m.get(p + 1..p + 1 + l) else {
                    bail!("label runs past the message")
                };
                total += l + 1;
                if total > 255 {
                    bail!("name longer than 255 bytes");
                }
                labels.push(String::from_utf8_lossy(bytes).to_ascii_lowercase());
                p += 1 + l;
            }
            _ => bail!("unsupported label type"),
        }
    }
    bail!("name has too many labels or pointers")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(owner: &[u8], t: u16, rdata: &[u8]) -> Vec<u8> {
        let mut r = owner.to_vec();
        r.extend_from_slice(&t.to_be_bytes());
        r.extend_from_slice(&C_IN.to_be_bytes());
        r.extend_from_slice(&60u32.to_be_bytes());
        r.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        r.extend_from_slice(rdata);
        r
    }

    /// A message with the question for mesh.test and the given answers;
    /// answers may point at the question name (offset 12).
    fn msg(id: u16, answers: &[Vec<u8>]) -> Vec<u8> {
        let mut m = query(id, "mesh.test").unwrap();
        m[2] = 0x84; // a response, authoritative
        m[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
        for a in answers {
            m.extend_from_slice(a);
        }
        m
    }

    const ZONE_PTR: [u8; 2] = [0xc0, 12];

    fn label_then_ptr(l: &str) -> Vec<u8> {
        let mut v = vec![l.len() as u8];
        v.extend_from_slice(l.as_bytes());
        v.extend_from_slice(&ZONE_PTR);
        v
    }

    #[test]
    fn a_records_between_the_two_soas() {
        let soa = rr(&ZONE_PTR, T_SOA, &[0; 22]);
        let m1 = msg(
            7,
            &[
                soa.clone(),
                rr(&label_then_ptr("DNS"), T_A, &[10, 23, 0, 11]),
            ],
        );
        let m2 = msg(
            7,
            &[rr(&label_then_ptr("registry"), T_A, &[10, 23, 0, 12]), soa],
        );
        let mut t = Transfer::default();
        assert!(!t.message(&m1, 7).unwrap());
        assert!(t.message(&m2, 7).unwrap());
        assert_eq!(
            t.a,
            vec![
                ("dns.mesh.test".to_string(), Ipv4Addr::new(10, 23, 0, 11)),
                (
                    "registry.mesh.test".to_string(),
                    Ipv4Addr::new(10, 23, 0, 12)
                ),
            ]
        );
    }

    #[test]
    fn refused_says_what_to_do() {
        let mut m = msg(1, &[]);
        m[3] = 5;
        let e = Transfer::default().message(&m, 1).unwrap_err().to_string();
        assert!(e.contains("REFUSED"), "{e}");
    }

    #[test]
    fn must_start_with_soa() {
        let m = msg(1, &[rr(&label_then_ptr("x"), T_A, &[10, 23, 0, 5])]);
        assert!(Transfer::default().message(&m, 1).is_err());
    }

    #[test]
    fn wrong_id_is_refused() {
        assert!(Transfer::default().message(&msg(2, &[]), 1).is_err());
    }

    #[test]
    fn pointer_loops_and_forward_pointers_are_refused() {
        // a pointer to itself
        let m = [0u8, 0, 0xc0, 2];
        assert!(name(&m, 2).is_err());
        // a pointer forward
        let m = [0xc0u8, 3, 0, 0];
        assert!(name(&m, 0).is_err());
    }

    #[test]
    fn non_a_records_and_other_classes_are_ignored() {
        let soa = rr(&ZONE_PTR, T_SOA, &[0; 22]);
        let mut chaos = rr(&label_then_ptr("c"), T_A, &[10, 23, 0, 7]);
        let n = chaos.len();
        chaos[n - 4 - 2 - 4 - 2..n - 4 - 2 - 4].copy_from_slice(&3u16.to_be_bytes()); // class CH
        let txt = rr(&label_then_ptr("t"), 16, b"\x03abc");
        let mut t = Transfer::default();
        assert!(
            t.message(&msg(9, &[soa.clone(), chaos, txt, soa]), 9)
                .unwrap()
        );
        assert!(t.a.is_empty());
    }

    #[test]
    fn server_must_be_an_address() {
        let s = |x: &str| DnsSource {
            zone: "z".into(),
            server: x.into(),
        };
        assert_eq!(s("10.23.0.1").addr().unwrap().port(), 53);
        assert_eq!(s("10.23.0.1:5353").addr().unwrap().port(), 5353);
        assert!(s("ns.example.org").addr().is_err());
    }

    /// Against a real server: XDEBBIE_TEST_AXFR=zone@server cargo test -- --ignored live_axfr
    #[tokio::test]
    #[ignore]
    async fn live_axfr() {
        let v = std::env::var("XDEBBIE_TEST_AXFR").expect("XDEBBIE_TEST_AXFR=zone@server");
        let (zone, server) = v.split_once('@').unwrap();
        let src = DnsSource {
            zone: zone.into(),
            server: server.into(),
        };
        let got = axfr(src.addr().unwrap(), zone, Duration::from_secs(10))
            .await
            .unwrap();
        for (n, ip) in &got {
            println!("{ip} {n}");
        }
        assert!(!got.is_empty());
    }

    /// A tiny AXFR server on loopback: the whole path, socket to set.
    #[tokio::test]
    async fn transfer_over_tcp_and_the_last_good_set_is_kept() {
        use tokio::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let serve = |l: TcpListener| async move {
            let (mut s, _) = l.accept().await.unwrap();
            let n = s.read_u16().await.unwrap() as usize;
            let mut q = vec![0u8; n];
            s.read_exact(&mut q).await.unwrap();
            let id = u16::from_be_bytes([q[0], q[1]]);
            let soa = rr(&ZONE_PTR, T_SOA, &[0; 22]);
            let m = msg(
                id,
                &[
                    soa.clone(),
                    rr(&label_then_ptr("api"), T_A, &[10, 23, 0, 9]),
                    rr(&label_then_ptr("far"), T_A, &[192, 168, 1, 1]),
                    soa,
                ],
            );
            s.write_all(&(m.len() as u16).to_be_bytes()).await.unwrap();
            s.write_all(&m).await.unwrap();
        };
        tokio::spawn(serve(l));
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("dns-reserved.json");
        let src = vec![DnsSource {
            zone: "mesh.test".into(),
            server: addr.to_string(),
        }];
        let cidr = Cidr::new(Ipv4Addr::new(10, 23, 0, 0), 16).unwrap();
        refresh(&src, cidr, &saved).await;
        let cur = current();
        assert_eq!(
            cur.get(&Ipv4Addr::new(10, 23, 0, 9)).map(String::as_str),
            Some("api.mesh.test")
        );
        assert!(
            !cur.contains_key(&Ipv4Addr::new(192, 168, 1, 1)),
            "outside the overlay"
        );
        assert!(saved.exists());
        // the server is gone now: the set stays, with the error noted
        refresh(&src, cidr, &saved).await;
        assert!(current().contains_key(&Ipv4Addr::new(10, 23, 0, 9)));
        assert!(status().zones["mesh.test"].error.is_some());
    }
}
