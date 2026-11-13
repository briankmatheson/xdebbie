// SPDX-License-Identifier: GPL-3.0-or-later
//! Find a cluster to join before bootstrapping a new one.
//!
//! Running daemons announce themselves on the discovery multicast group
//! every few seconds (`discovery::engine`). A fresh `xdebbied bootstrap`
//! with no `--join` listens for a while; if anybody is out there it offers
//! to join them, otherwise it asks for a URL and, failing that, starts a
//! new cluster (this box becomes the CA).

use std::{collections::BTreeMap, time::{Duration, Instant}};

use anyhow::{Result, bail};
use tracing::info;
use uuid::Uuid;

use crate::discovery::{messages::DiscoveryMessage, multicast::MulticastSocket};

/// Default listening time. Announces go out every 5 s, so this hears a
/// running cluster several times over.
pub const DEFAULT_LISTEN_SECS: u64 = 20;
/// Once something is heard, keep listening this much longer so the
/// leader's heartbeat has a chance to arrive.
const SETTLE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub hostname: String,
    pub api_url: String,
    pub is_leader: bool,
}

/// Listen on `multicast_addr` for up to `timeout`. Returns what was heard,
/// leader first, our own hostname excluded.
pub fn listen(multicast_addr: &str, timeout: Duration, own_hostname: &str) -> Result<Vec<Found>> {
    let port = multicast_addr.rsplit(':').next().unwrap_or("7442");
    let sock = MulticastSocket::new(multicast_addr, &format!("0.0.0.0:{port}"))?;
    let start = Instant::now();
    let mut first_heard: Option<Instant> = None;
    let mut nodes: BTreeMap<Uuid, Found> = BTreeMap::new();
    let mut leaders: Vec<Uuid> = vec![];
    while start.elapsed() < timeout {
        match sock.recv()? {
            Some((DiscoveryMessage::Announce { node_id, hostname, api_url, addrs, .. }, src)) => {
                if hostname != own_hostname {
                    nodes.entry(node_id).or_insert_with(|| {
                        let candidates = crate::discovery::reach::source_ip(src).into_iter().chain(addrs);
                        let api_url = crate::discovery::reach::reachable_url(&api_url, candidates);
                        info!(host = %hostname, url = %api_url, "heard a running xdebbied");
                        Found { hostname, api_url, is_leader: false }
                    });
                    first_heard.get_or_insert_with(Instant::now);
                }
            }
            Some((DiscoveryMessage::Heartbeat { node_id, is_leader: true, .. }, _)) => {
                if !leaders.contains(&node_id) {
                    leaders.push(node_id);
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
        if let Some(t) = first_heard
            && t.elapsed() >= SETTLE
            && nodes.keys().any(|id| leaders.contains(id))
        {
            break;
        }
    }
    let mut out: Vec<Found> = nodes
        .into_iter()
        .map(|(id, mut f)| {
            f.is_leader = leaders.contains(&id);
            f
        })
        .collect();
    out.sort_by_key(|f| (!f.is_leader, f.hostname.clone()));
    Ok(out)
}

/// What bootstrap decided to do about other boxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Enrol with the cluster reachable at this URL.
    Join(String),
    /// Start a new cluster here.
    First,
}

fn is_tty() -> bool {
    // SAFETY: plain libc query on fd 0.
    unsafe { libc::isatty(0) == 1 }
}

fn ask(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Listen, then decide: with a terminal, confirm or ask for a URL; without
/// one, a found cluster is an error (never silently start a second cluster
/// next to a running one), silence means first box.
pub fn choose(multicast_addr: &str, timeout: Duration, own_hostname: &str) -> Result<Choice> {
    if timeout.is_zero() {
        return Ok(Choice::First);
    }
    info!(
        "Listening {}s on {multicast_addr} for a running cluster (--join <url> or --standalone to skip)",
        timeout.as_secs()
    );
    let found = match listen(multicast_addr, timeout, own_hostname) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("discovery listen failed ({e:#}); treating as no cluster found");
            vec![]
        }
    };
    let tty = is_tty();
    if let Some(best) = found.first() {
        eprintln!();
        eprintln!("Found a running cluster:");
        for f in &found {
            eprintln!("  {:<28} {}{}", f.hostname, f.api_url, if f.is_leader { "  (leader)" } else { "" });
        }
        if !tty {
            bail!(
                "a cluster is already running ({} at {}). Re-run with `xdebbied bootstrap --join {}` to enrol \
                 this box, or `--standalone` to start a separate cluster anyway",
                best.hostname, best.api_url, best.api_url
            );
        }
        let a = ask(&format!("Join it via {}? [Y/n/other URL] ", best.api_url))?;
        return Ok(match a.as_str() {
            "" | "y" | "Y" | "yes" => Choice::Join(best.api_url.clone()),
            "n" | "N" | "no" => Choice::First,
            url if url.starts_with("https://") => Choice::Join(url.to_string()),
            other => bail!("expected y, n or an https:// URL, got {other:?}"),
        });
    }
    if !tty {
        info!("No cluster heard; this box starts one");
        return Ok(Choice::First);
    }
    eprintln!();
    eprintln!("No running cluster heard on {multicast_addr}.");
    let a = ask("URL of a box to join (https://host:7443), or Enter if this is the first box: ")?;
    Ok(match a.as_str() {
        "" => Choice::First,
        url if url.starts_with("https://") => Choice::Join(url.to_string()),
        other => bail!("expected an https:// URL or nothing, got {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::messages::NodeCapacity;

    #[test]
    fn hears_announces_and_marks_the_leader() {
        // The real discovery port: a host firewall that lets xdebbied talk
        // lets this through too (an odd port would be dropped). A daemon
        // running on the test box is simply heard as one more node.
        let addr = crate::discovery::multicast::DEFAULT_MULTICAST_ADDR;
        let tx = MulticastSocket::new(addr, "0.0.0.0:7442").unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let cap = NodeCapacity { total_cpus: 1, total_memory_mib: 1, available_cpus: 1.0, available_memory_mib: 1, vm_count: 0, control_plane_eligible: false };
        let sender = std::thread::spawn(move || {
            for _ in 0..30 {
                let _ = tx.send(&DiscoveryMessage::Announce { node_id: a, hostname: "alpha".into(), api_url: "https://192.0.2.1:7443".into(), addrs: vec![], capacity: cap.clone(), score: 1.0, sequence: 1 });
                let _ = tx.send(&DiscoveryMessage::Announce { node_id: b, hostname: "beta".into(), api_url: "https://192.0.2.2:7443".into(), addrs: vec![], capacity: cap.clone(), score: 1.0, sequence: 1 });
                let _ = tx.send(&DiscoveryMessage::Announce { node_id: Uuid::new_v4(), hostname: "me".into(), api_url: "https://me:7443".into(), addrs: vec![], capacity: cap.clone(), score: 1.0, sequence: 1 });
                let _ = tx.send(&DiscoveryMessage::Heartbeat { node_id: b, score: 1.0, is_leader: true, sequence: 2 });
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let found = listen(addr, Duration::from_secs(10), "me").unwrap();
        sender.join().unwrap();
        let get = |h: &str| found.iter().find(|f| f.hostname == h).unwrap_or_else(|| panic!("{h} not heard: {found:?}")).clone();
        assert!(!found.iter().any(|f| f.hostname == "me"));
        assert_eq!(get("beta"), Found { hostname: "beta".into(), api_url: "https://192.0.2.2:7443".into(), is_leader: true });
        assert_eq!(get("alpha"), Found { hostname: "alpha".into(), api_url: "https://192.0.2.1:7443".into(), is_leader: false });
        // Leaders sort first.
        let bi = found.iter().position(|f| f.hostname == "beta").unwrap();
        let ai = found.iter().position(|f| f.hostname == "alpha").unwrap();
        assert!(bi < ai, "{found:?}");
    }

    #[test]
    fn zero_timeout_means_first_box() {
        assert_eq!(choose("239.0.0.42:17443", Duration::ZERO, "me").unwrap(), Choice::First);
    }
}
