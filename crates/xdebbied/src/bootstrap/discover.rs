// SPDX-License-Identifier: GPL-3.0-or-later
//! Find a cluster to join before bootstrapping a new one.
//!
//! Running daemons announce themselves on the discovery multicast group
//! every few seconds (`discovery::engine`). A fresh `xdebbied bootstrap`
//! with no `--join` listens for a while; if anybody is out there it offers
//! to join them, otherwise it asks for a URL and, failing that, starts a
//! new cluster (this box becomes the CA).

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use tracing::info;
use uuid::Uuid;

use crate::discovery::{
    messages::DiscoveryMessage,
    multicast::{MulticastSocket, Transport},
};

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
    let port = multicast_addr.rsplit(':').next().unwrap_or("23442");
    let sock = MulticastSocket::new(multicast_addr, &format!("0.0.0.0:{port}"))?;
    listen_on(
        &sock,
        &WallClock(Instant::now()),
        crate::discovery::reach::resolves,
        timeout,
        own_hostname,
    )
}

/// Time as [`listen_on`] sees it: the real clock, or a test's.
pub trait Clock {
    /// Time since listening started.
    fn elapsed(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

struct WallClock(Instant);

impl Clock for WallClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// [`listen`] on any transport, clock and resolver.
pub fn listen_on(
    sock: &dyn Transport,
    clock: &dyn Clock,
    resolves: impl Fn(&str) -> bool,
    timeout: Duration,
    own_hostname: &str,
) -> Result<Vec<Found>> {
    let mut first_heard: Option<Duration> = None;
    let mut nodes: BTreeMap<Uuid, Found> = BTreeMap::new();
    let mut leaders: Vec<Uuid> = vec![];
    while clock.elapsed() < timeout {
        // Before a join this box holds no token, so nothing can be verified:
        // what is heard is only offered as a URL, and the join is what gets
        // trusted. Unreadable datagrams are skipped.
        let heard = match sock.recv()? {
            Some((bytes, src)) => match crate::discovery::wire::peek(&bytes) {
                Ok(m) => Some((m, src)),
                Err(_) => continue,
            },
            None => None,
        };
        match heard {
            Some((
                DiscoveryMessage::Announce {
                    node_id,
                    hostname,
                    api_url,
                    addrs,
                    ..
                },
                src,
            )) => {
                if hostname != own_hostname {
                    nodes.entry(node_id).or_insert_with(|| {
                        let candidates = crate::discovery::reach::source_ip(src)
                            .into_iter()
                            .chain(addrs);
                        let api_url = crate::discovery::reach::reachable_url_with(
                            &api_url, candidates, &resolves,
                        );
                        info!(host = %hostname, url = %api_url, "heard a running xdebbied");
                        Found {
                            hostname,
                            api_url,
                            is_leader: false,
                        }
                    });
                    first_heard.get_or_insert_with(|| clock.elapsed());
                }
            }
            Some((
                DiscoveryMessage::Heartbeat {
                    node_id,
                    is_leader: true,
                    ..
                },
                _,
            )) => {
                if !leaders.contains(&node_id) {
                    leaders.push(node_id);
                }
            }
            Some(_) => {}
            None => clock.sleep(Duration::from_millis(100)),
        }
        if let Some(t) = first_heard
            && clock.elapsed() >= t + SETTLE
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
    decide(multicast_addr, &found, is_tty(), ask)
}

/// What to do about `found`: the decision half of [`choose`]. `ask` puts a
/// question to the operator; only called when `tty`.
pub fn decide(
    multicast_addr: &str,
    found: &[Found],
    tty: bool,
    mut ask: impl FnMut(&str) -> Result<String>,
) -> Result<Choice> {
    if let Some(best) = found.first() {
        eprintln!();
        eprintln!("Found a running cluster:");
        for f in found {
            eprintln!(
                "  {:<28} {}{}",
                f.hostname,
                f.api_url,
                if f.is_leader { "  (leader)" } else { "" }
            );
        }
        if !tty {
            bail!(
                "a cluster is already running ({} at {}). Re-run with `xdebbied bootstrap --join {}` to enrol \
                 this box, or `--standalone` to start a separate cluster anyway",
                best.hostname,
                best.api_url,
                best.api_url
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
    let a = ask("URL of a box to join (https://host:23443), or Enter if this is the first box: ")?;
    Ok(match a.as_str() {
        "" => Choice::First,
        url if url.starts_with("https://") => Choice::Join(url.to_string()),
        other => bail!("expected an https:// URL or nothing, got {other:?}"),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::discovery::messages::NodeCapacity;
    use std::{
        net::SocketAddr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    /// A hand-advanced clock: `sleep` moves it, nothing else does.
    #[derive(Clone, Default)]
    pub(crate) struct FakeClock(Arc<AtomicU64>);

    impl Clock for FakeClock {
        fn elapsed(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
        fn sleep(&self, d: Duration) {
            self.0.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        }
    }

    /// Datagrams that arrive at set times on [`FakeClock`]: a recording of
    /// the air on the discovery group.
    pub(crate) struct Air {
        clock: FakeClock,
        due: Mutex<std::collections::VecDeque<(Duration, Vec<u8>, SocketAddr)>>,
    }

    impl Air {
        pub(crate) fn new(clock: &FakeClock, mut v: Vec<(Duration, Vec<u8>, SocketAddr)>) -> Self {
            v.sort_by_key(|(t, ..)| *t);
            Self {
                clock: clock.clone(),
                due: Mutex::new(v.into()),
            }
        }
    }

    impl Transport for Air {
        fn send(&self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn recv(&self) -> Result<Option<(Vec<u8>, SocketAddr)>> {
            let mut q = self.due.lock().unwrap();
            if q.front().is_some_and(|(t, ..)| *t <= self.clock.elapsed()) {
                let (_, b, src) = q.pop_front().unwrap();
                return Ok(Some((b, src)));
            }
            Ok(None)
        }
    }

    fn cap() -> NodeCapacity {
        NodeCapacity {
            total_cpus: 1,
            total_memory_mib: 1,
            available_cpus: 1.0,
            available_memory_mib: 1,
            vm_count: 0,
            control_plane_eligible: false,
        }
    }

    fn announce(id: Uuid, host: &str, url: &str, addrs: Vec<std::net::IpAddr>) -> DiscoveryMessage {
        DiscoveryMessage::Announce {
            node_id: id,
            hostname: host.into(),
            api_url: url.into(),
            addrs,
            capacity: cap(),
            score: 1.0,
            sequence: 1,
        }
    }

    fn heartbeat(id: Uuid, is_leader: bool) -> DiscoveryMessage {
        DiscoveryMessage::Heartbeat {
            node_id: id,
            score: 1.0,
            is_leader,
            sequence: 2,
        }
    }

    /// Signed with a token this listener does not hold, as a running
    /// cluster's datagrams are.
    fn signed(m: &DiscoveryMessage) -> Vec<u8> {
        let key = crate::discovery::wire::Key::from_token("cluster token");
        crate::discovery::wire::seal(m, Some(&key)).unwrap()
    }

    fn src(last: u8) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, last], 23442))
    }

    /// A box on the air: announces every 5 s and heartbeats every second
    /// from `from`, as `discovery::engine` does.
    fn cluster_box(
        from: Duration,
        id: Uuid,
        host: &str,
        url: &str,
        leader: bool,
        at: SocketAddr,
    ) -> Vec<(Duration, Vec<u8>, SocketAddr)> {
        let mut v = vec![];
        for s in 0..60u64 {
            let t = from + Duration::from_secs(s);
            if s % 5 == 0 {
                v.push((t, signed(&announce(id, host, url, vec![])), at));
            }
            v.push((t, signed(&heartbeat(id, leader)), at));
        }
        v
    }

    fn never_resolves(_: &str) -> bool {
        false
    }

    #[test]
    fn hears_announces_and_marks_the_leader() {
        let clock = FakeClock::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let t = Duration::from_secs(2);
        let mut air = cluster_box(t, a, "alpha", "https://192.0.2.1:23443", false, src(1));
        air.extend(cluster_box(
            t,
            b,
            "beta",
            "https://192.0.2.2:23443",
            true,
            src(2),
        ));
        // A box with our own hostname (a stale daemon on this very box).
        air.extend(cluster_box(
            t,
            Uuid::new_v4(),
            "me",
            "https://me:23443",
            false,
            src(3),
        ));
        let air = Air::new(&clock, air);
        let found = listen_on(
            &air,
            &clock,
            never_resolves,
            Duration::from_secs(DEFAULT_LISTEN_SECS),
            "me",
        )
        .unwrap();
        assert_eq!(
            found,
            [
                Found {
                    hostname: "beta".into(),
                    api_url: "https://192.0.2.2:23443".into(),
                    is_leader: true
                },
                Found {
                    hostname: "alpha".into(),
                    api_url: "https://192.0.2.1:23443".into(),
                    is_leader: false
                },
            ],
            "leader first, own hostname left out"
        );
        // Heard at 2 s, leader confirmed, then SETTLE: done well before 20 s.
        let took = clock.elapsed();
        assert!(
            took >= t + SETTLE && took < t + SETTLE + Duration::from_secs(1),
            "{took:?}"
        );
    }

    #[test]
    fn hears_nothing_listens_the_whole_time() {
        let clock = FakeClock::default();
        let air = Air::new(&clock, vec![]);
        let timeout = Duration::from_secs(DEFAULT_LISTEN_SECS);
        let found = listen_on(&air, &clock, never_resolves, timeout, "me").unwrap();
        assert!(found.is_empty());
        assert!(clock.elapsed() >= timeout, "{:?}", clock.elapsed());
        assert!(clock.elapsed() < timeout + Duration::from_secs(1));
    }

    /// Announces but no leader heartbeat (an election running): keep
    /// listening to the end, then offer what was heard.
    #[test]
    fn no_leader_heard_listens_to_the_end() {
        let clock = FakeClock::default();
        let id = Uuid::new_v4();
        let air = Air::new(
            &clock,
            cluster_box(
                Duration::from_secs(1),
                id,
                "alpha",
                "https://192.0.2.1:23443",
                false,
                src(1),
            ),
        );
        let timeout = Duration::from_secs(DEFAULT_LISTEN_SECS);
        let found = listen_on(&air, &clock, never_resolves, timeout, "me").unwrap();
        assert_eq!(found.len(), 1);
        assert!(!found[0].is_leader);
        assert!(clock.elapsed() >= timeout);
    }

    /// A leader heartbeat that arrives before its announce still marks it.
    #[test]
    fn heartbeat_before_announce_still_marks_the_leader() {
        let clock = FakeClock::default();
        let id = Uuid::new_v4();
        let air = Air::new(
            &clock,
            vec![
                (Duration::from_secs(1), signed(&heartbeat(id, true)), src(1)),
                (
                    Duration::from_secs(4),
                    signed(&announce(id, "alpha", "https://192.0.2.1:23443", vec![])),
                    src(1),
                ),
            ],
        );
        let found = listen_on(&air, &clock, never_resolves, Duration::from_secs(20), "me").unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].is_leader);
    }

    #[test]
    fn garbage_and_plain_datagrams() {
        let clock = FakeClock::default();
        let id = Uuid::new_v4();
        let t = Duration::from_millis(500);
        let air = Air::new(
            &clock,
            vec![
                (t, b"not json".to_vec(), src(9)),
                (t, b"{\"mac\":\"zz\"}".to_vec(), src(9)),
                (t, vec![0xff, 0xfe], src(9)),
                (t, br#"{"type":"NOPE"}"#.to_vec(), src(9)),
                // An unsigned announce (a cluster without a token) is heard too.
                (
                    t,
                    announce(id, "plain", "https://192.0.2.5:23443", vec![])
                        .to_json()
                        .unwrap()
                        .into_bytes(),
                    src(5),
                ),
            ],
        );
        let found = listen_on(&air, &clock, never_resolves, Duration::from_secs(5), "me").unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].hostname, "plain");
    }

    /// A name nobody here resolves becomes the datagram's source address,
    /// so the join URL offered is one this box can dial.
    #[test]
    fn unresolvable_join_url_uses_the_source_address() {
        let clock = FakeClock::default();
        let id = Uuid::new_v4();
        let air = Air::new(
            &clock,
            vec![(
                Duration::ZERO,
                signed(&announce(
                    id,
                    "node0",
                    "https://node0.example.net:23443",
                    vec!["198.51.100.4".parse().unwrap()],
                )),
                src(4),
            )],
        );
        let found = listen_on(&air, &clock, never_resolves, Duration::from_secs(5), "me").unwrap();
        assert_eq!(found[0].api_url, "https://192.0.2.4:23443");

        // Where it does resolve, the name stays.
        let clock = FakeClock::default();
        let air = Air::new(
            &clock,
            vec![(
                Duration::ZERO,
                signed(&announce(
                    id,
                    "node0",
                    "https://node0.example.net:23443",
                    vec![],
                )),
                src(4),
            )],
        );
        let found = listen_on(&air, &clock, |_| true, Duration::from_secs(5), "me").unwrap();
        assert_eq!(found[0].api_url, "https://node0.example.net:23443");
    }

    fn found(host: &str, url: &str, leader: bool) -> Found {
        Found {
            hostname: host.into(),
            api_url: url.into(),
            is_leader: leader,
        }
    }

    fn no_tty(_: &str) -> Result<String> {
        panic!("asked a question without a terminal")
    }

    /// The safety rule: without a terminal, a running cluster is never
    /// joined or shadowed silently; the error says how to join it.
    #[test]
    fn heard_a_cluster_refuses_to_found_a_second() {
        let heard = [
            found("beta", "https://192.0.2.2:23443", true),
            found("alpha", "https://192.0.2.1:23443", false),
        ];
        let e = decide("239.0.0.42:23442", &heard, false, no_tty)
            .unwrap_err()
            .to_string();
        assert!(e.contains("already running"), "{e}");
        assert!(
            e.contains("--join https://192.0.2.2:23443"),
            "names the leader's URL: {e}"
        );
        assert!(e.contains("--standalone"), "{e}");
    }

    #[test]
    fn heard_nothing_founds_a_cluster() {
        assert_eq!(
            decide("239.0.0.42:23442", &[], false, no_tty).unwrap(),
            Choice::First
        );
    }

    #[test]
    fn terminal_answers() {
        let heard = [found("beta", "https://192.0.2.2:23443", true)];
        let with = |a: &'static str| {
            decide("239.0.0.42:23442", &heard, true, move |q: &str| {
                assert!(q.contains("https://192.0.2.2:23443"), "{q}");
                Ok(a.to_string())
            })
        };
        for yes in ["", "y", "Y", "yes"] {
            assert_eq!(
                with(yes).unwrap(),
                Choice::Join("https://192.0.2.2:23443".into())
            );
        }
        for no in ["n", "N", "no"] {
            assert_eq!(with(no).unwrap(), Choice::First);
        }
        assert_eq!(
            with("https://other:23443").unwrap(),
            Choice::Join("https://other:23443".into())
        );
        assert!(with("http://plain:23443").is_err());
        assert!(with("maybe").is_err());

        let alone = |a: &'static str| decide("x", &[], true, move |_: &str| Ok(a.to_string()));
        assert_eq!(alone("").unwrap(), Choice::First);
        assert_eq!(
            alone("https://n:23443").unwrap(),
            Choice::Join("https://n:23443".into())
        );
        assert!(alone("n").is_err());
    }

    #[test]
    fn zero_timeout_means_first_box() {
        assert_eq!(
            choose("239.0.0.42:17443", Duration::ZERO, "me").unwrap(),
            Choice::First
        );
    }

    /// The same over real multicast. Needs a host whose kernel loops
    /// multicast back and a firewall that lets 23442/udp through.
    /// `XDEBBIE_TEST_MULTICAST=1 cargo test -p xdebbied -- --ignored multicast`
    #[test]
    #[ignore = "real multicast; XDEBBIE_TEST_MULTICAST=1"]
    fn multicast_hears_announces_and_marks_the_leader() {
        if std::env::var_os("XDEBBIE_TEST_MULTICAST").is_none() {
            eprintln!("XDEBBIE_TEST_MULTICAST unset; skipping");
            return;
        }
        // The real discovery port, so a host firewall that lets xdebbied talk
        // lets this through too, but a private group: a daemon on this box
        // or the LAN only receives the group it joined (IP_MULTICAST_ALL is
        // off), so "alpha", "beta" and "me" never show up as its peers.
        let addr = "239.0.0.43:23442";
        let tx = MulticastSocket::new(addr, "0.0.0.0:23442").unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sender = std::thread::spawn(move || {
            for _ in 0..30 {
                for m in [
                    announce(a, "alpha", "https://192.0.2.1:23443", vec![]),
                    announce(b, "beta", "https://192.0.2.2:23443", vec![]),
                    announce(Uuid::new_v4(), "me", "https://me:23443", vec![]),
                    heartbeat(b, true),
                ] {
                    let _ = tx.send(&signed(&m));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let heard = listen(addr, Duration::from_secs(10), "me").unwrap();
        sender.join().unwrap();
        assert_eq!(
            heard,
            [
                found("beta", "https://192.0.2.2:23443", true),
                found("alpha", "https://192.0.2.1:23443", false)
            ]
        );
    }
}
