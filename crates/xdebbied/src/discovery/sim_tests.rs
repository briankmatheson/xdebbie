// SPDX-License-Identifier: GPL-3.0-or-later
//! Discovery, announces and leader election on a simulated LAN (sim.rs):
//! real engines, in-memory bus, paused clock. The timings asserted are the
//! ones docs/autodiscovery.md promises.

use std::time::Duration;

use super::{
    messages::DiscoveryMessage,
    sim::{Lan, SimBox, Spec, TOKEN, leaders, run_for, secs, settle, settled, watch},
    wire,
};
use crate::election::state::ElectionState;

fn all(v: &[SimBox]) -> Vec<&SimBox> {
    v.iter().collect()
}

/// The frames `b` sent, decoded (unverified).
fn sent_by(lan: &Lan, b: &SimBox) -> Vec<(Duration, Vec<u8>, DiscoveryMessage)> {
    lan.log()
        .into_iter()
        .filter(|f| f.from == b.idx)
        .map(|f| {
            let m = wire::peek(&f.bytes).unwrap();
            (f.at, f.bytes, m)
        })
        .collect()
}

/// A signed envelope's inner message as JSON, for tampering.
fn edit_envelope(bytes: &[u8], f: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut env: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let mut msg: serde_json::Value = serde_json::from_str(env["msg"].as_str().unwrap()).unwrap();
    f(&mut msg);
    env["msg"] = serde_json::Value::String(msg.to_string());
    serde_json::to_vec(&env).unwrap()
}

fn signed(m: &DiscoveryMessage, token: &str, ts: u64) -> Vec<u8> {
    wire::seal_at(m, Some(&wire::Key::from_token(token)), ts).unwrap()
}

fn forged_leader(rank: u8, seq: u64) -> DiscoveryMessage {
    DiscoveryMessage::Heartbeat {
        node_id: super::sim::id(rank),
        score: 1e9,
        is_leader: true,
        sequence: seq,
    }
}

fn announce_of(rank: u8, name: &str, seq: u64) -> DiscoveryMessage {
    DiscoveryMessage::Announce {
        node_id: super::sim::id(rank),
        hostname: name.into(),
        api_url: "https://192.0.2.99:23443".into(),
        addrs: vec![],
        capacity: super::messages::NodeCapacity {
            total_cpus: 1,
            total_memory_mib: 1,
            available_cpus: 1.0,
            available_memory_mib: 1,
            vm_count: 0,
            control_plane_eligible: true,
        },
        score: 1.0,
        sequence: seq,
    }
}

// ---------- announces: discovery and authentication ----------

#[tokio::test(start_paused = true)]
async fn boxes_find_each_other_and_rewrite_unresolvable_urls() {
    let lan = Lan::new();
    let v = [
        lan.spawn(Spec::new("a", 1)),
        lan.spawn(Spec::new("b", 2)),
        lan.spawn(Spec::new("c", 3)),
    ];
    run_for(secs(6)).await;
    assert_eq!(v[0].peer_names().await, ["b", "c"]);
    assert_eq!(v[1].peer_names().await, ["a", "c"]);
    // Each announced `https://<name>.invalid:23443`; nobody resolves that,
    // so peers dial the datagram's source address instead.
    let urls: Vec<String> = v[0]
        .leadership
        .peers()
        .await
        .into_iter()
        .map(|p| p.api_url)
        .collect();
    assert!(urls.contains(&v[1].api_url), "{urls:?}");
    assert!(urls.contains(&v[2].api_url), "{urls:?}");
}

#[tokio::test(start_paused = true)]
async fn everything_sent_is_signed_and_on_schedule() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    run_for(secs(20)).await;
    let key = wire::Key::from_token(TOKEN);
    let mut announces = 0;
    let mut heartbeats = 0;
    for (at, bytes, m) in sent_by(&lan, &a) {
        assert!(
            matches!(
                wire::open_at(&bytes, std::slice::from_ref(&key), lan.wall()).unwrap(),
                wire::Opened::Verified(_)
            ),
            "unsigned or unverifiable frame at {at:?}"
        );
        match m {
            DiscoveryMessage::Announce { .. } => announces += 1,
            DiscoveryMessage::Heartbeat { .. } => heartbeats += 1,
            _ => {}
        }
    }
    // Initial announce plus one every 5 s; a heartbeat every second.
    assert!(
        (4..=6).contains(&announces),
        "{announces} announces in 20 s"
    );
    assert!(
        (19..=22).contains(&heartbeats),
        "{heartbeats} heartbeats in 20 s"
    );
}

#[tokio::test(start_paused = true)]
async fn sequence_numbers_rise() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    run_for(secs(10)).await;
    let seqs: Vec<u64> = sent_by(&lan, &a)
        .iter()
        .map(|(.., m)| m.sequence())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
}

#[tokio::test(start_paused = true)]
async fn wrong_token_is_not_heard() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2));
    let stranger = lan.spawn(Spec::new("stranger", 9).token("some other cluster", None));
    run_for(secs(30)).await;
    assert_eq!(a.peer_names().await, ["b"]);
    assert_eq!(b.peer_names().await, ["a"]);
    assert!(stranger.peer_names().await.is_empty());
    // The stranger has the best tiebreak but nobody hears it: a and b elect
    // between themselves, and it leads alone.
    assert_eq!(settled(&[&a, &b]).await.as_deref(), Some("b"));
    assert!(stranger.is_leader().await);
}

/// A forged heartbeat claiming leadership with a huge score must not make
/// the real leader step down or redirect followers.
#[tokio::test(start_paused = true)]
async fn forged_leader_heartbeat_ignored() {
    let lan = Lan::new();
    let v = vec![lan.spawn(Spec::new("a", 1)), lan.spawn(Spec::new("b", 2))];
    let (leader, _) = settle(&all(&v), secs(20)).await;
    for s in 1..=20u64 {
        for b in &v {
            // Signed with the wrong token.
            lan.inject(
                b,
                signed(&forged_leader(200, s), "guess", lan.wall()),
                b.src(),
            );
            // Not signed at all.
            lan.inject(
                b,
                forged_leader(201, s).to_json().unwrap().into_bytes(),
                b.src(),
            );
        }
        run_for(secs(1)).await;
    }
    assert_eq!(settled(&all(&v)).await, Some(leader));
    for b in &v {
        assert_eq!(b.peer_names().await.len(), 1);
    }
}

/// A real follower's heartbeat, edited to claim leadership and a better
/// score: the MAC no longer matches and the edit goes nowhere.
#[tokio::test(start_paused = true)]
async fn tampered_fields_rejected() {
    let lan = Lan::new();
    let v = vec![lan.spawn(Spec::new("a", 1)), lan.spawn(Spec::new("b", 2))];
    let (leader, _) = settle(&all(&v), secs(20)).await;
    let (l, f) = if leader == "a" {
        (&v[0], &v[1])
    } else {
        (&v[1], &v[0])
    };
    let (_, bytes, _) = sent_by(&lan, f)
        .into_iter()
        .rev()
        .find(|(.., m)| matches!(m, DiscoveryMessage::Heartbeat { .. }))
        .unwrap();
    let forged = edit_envelope(&bytes, |m| {
        m["is_leader"] = true.into();
        m["score"] = 1e9.into();
        m["sequence"] = 1_000_000.into();
    });
    // And an announce with a rewritten URL, to hijack forwarding.
    let (_, abytes, _) = sent_by(&lan, f)
        .into_iter()
        .rev()
        .find(|(.., m)| matches!(m, DiscoveryMessage::Announce { .. }))
        .unwrap();
    let hijack = edit_envelope(&abytes, |m| {
        m["node_id"] = super::sim::id(77).to_string().into();
        m["api_url"] = "https://198.51.100.66:23443".into();
    });
    for _ in 0..10 {
        lan.inject(l, forged.clone(), f.src());
        lan.inject(l, hijack.clone(), f.src());
        run_for(secs(1)).await;
    }
    assert!(l.is_leader().await);
    assert_eq!(settled(&all(&v)).await, Some(leader));
    assert_eq!(
        l.peer_names().await.len(),
        1,
        "forged announce added a peer"
    );
}

/// Datagrams stamped more than 30 s from the receiver's clock are dropped;
/// within that they are heard.
#[tokio::test(start_paused = true)]
async fn stale_and_future_announces_rejected() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    run_for(secs(1)).await;
    let w = lan.wall();
    let src = "192.0.2.99:23442".parse().unwrap();
    lan.inject(&a, signed(&announce_of(50, "old", 1), TOKEN, w - 31), src);
    lan.inject(
        &a,
        signed(&announce_of(51, "future", 1), TOKEN, w + 31),
        src,
    );
    lan.inject(&a, signed(&announce_of(52, "late", 1), TOKEN, w - 29), src);
    lan.inject(&a, signed(&announce_of(53, "early", 1), TOKEN, w + 29), src);
    run_for(secs(1)).await;
    assert_eq!(a.peer_names().await, ["early", "late"]);
}

/// A dead box's captured announce, replayed inside the clock window after
/// it has been dropped as stale, must not bring it back.
#[tokio::test(start_paused = true)]
async fn replayed_announce_rejected() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2));
    run_for(secs(6)).await;
    assert_eq!(a.peer_names().await, ["b"]);
    let (at, bytes, _) = sent_by(&lan, &b)
        .into_iter()
        .rev()
        .find(|(.., m)| matches!(m, DiscoveryMessage::Announce { .. }))
        .unwrap();
    b.kill();
    lan.unplug(&b);
    // Stale 15 s after its last word, swept by the 10 s cleanup after that.
    run_for(secs(25)).await;
    assert!(a.peer_names().await.is_empty(), "dead peer not dropped");
    assert!(
        lan.now() - at < wire::MAX_SKEW,
        "replay still inside the window"
    );
    lan.inject(&a, bytes, b.src());
    run_for(secs(1)).await;
    assert!(
        a.peer_names().await.is_empty(),
        "a replayed announce resurrected a dead peer"
    );
}

/// Replaying a dead leader's captured heartbeats must not hold off the
/// re-election.
#[tokio::test(start_paused = true)]
async fn replayed_leader_heartbeats_do_not_hold_off_reelection() {
    let lan = Lan::new();
    let v = vec![
        lan.spawn(Spec::new("a", 1)),
        lan.spawn(Spec::new("b", 2)),
        lan.spawn(Spec::new("c", 3)),
    ];
    let (leader, _) = settle(&all(&v), secs(20)).await;
    assert_eq!(leader, "c");
    run_for(secs(5)).await;
    let beats: Vec<Vec<u8>> = sent_by(&lan, &v[2])
        .into_iter()
        .filter(|(.., m)| {
            matches!(
                m,
                DiscoveryMessage::Heartbeat {
                    is_leader: true,
                    ..
                }
            )
        })
        .map(|(_, b, _)| b)
        .collect();
    let last = beats.last().unwrap().clone();
    v[2].kill();
    lan.unplug(&v[2]);
    let t0 = lan.now();
    // The attacker replays the leader's last heartbeat every second.
    let rest = [&v[0], &v[1]];
    loop {
        for b in rest {
            lan.inject(b, last.clone(), v[2].src());
        }
        run_for(secs(1)).await;
        if settled(&rest).await.is_some() || lan.now() - t0 > secs(40) {
            break;
        }
    }
    let took = lan.now() - t0;
    assert!(took <= secs(17), "re-election held off {took:?} by replays");
}

#[tokio::test(start_paused = true)]
async fn clock_skew_within_window_tolerated() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2).skew(25));
    let c = lan.spawn(Spec::new("c", 3).skew(-25));
    // a sees both within 25 s; b and c are 50 s apart and do not hear each
    // other, but both hear a.
    run_for(secs(6)).await;
    assert_eq!(a.peer_names().await, ["b", "c"]);
    assert_eq!(b.peer_names().await, ["a"]);
    assert_eq!(c.peer_names().await, ["a"]);
}

/// Clocks further apart than the window: neither hears the other and each
/// leads alone (the documented failure mode, not a crash).
#[tokio::test(start_paused = true)]
async fn clock_skew_beyond_window_splits() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2).skew(40));
    run_for(secs(20)).await;
    assert!(a.peer_names().await.is_empty());
    assert!(b.peer_names().await.is_empty());
    assert_eq!(leaders(&[&a, &b]).await, ["a", "b"]);
    // NTP fixes b's clock: they find each other and settle on one leader.
    b.set_skew(0);
    let (l, _) = settle(&[&a, &b], secs(15)).await;
    assert!(l == "a" || l == "b");
}

#[tokio::test(start_paused = true)]
async fn unsigned_only_with_accept_unsigned() {
    let lan = Lan::new();
    let old = lan.spawn(Spec::new("old", 1).no_token());
    let strict = lan.spawn(Spec::new("strict", 2));
    let upgrading = lan.spawn(Spec::new("upgrading", 3).accept_unsigned());
    run_for(secs(6)).await;
    assert!(strict.peer_names().await.contains(&"upgrading".to_string()));
    assert!(!strict.peer_names().await.contains(&"old".to_string()));
    assert!(upgrading.peer_names().await.contains(&"old".to_string()));
    // A tokenless box cannot verify envelopes, so it hears no signed box.
    assert!(old.peer_names().await.is_empty());
}

/// Mid-rotation (docs: senders sign with the previous token) a box that
/// already has the new token and one that has only the old one still hear
/// each other.
#[tokio::test(start_paused = true)]
async fn token_rotation_in_flight_keeps_the_cluster() {
    let lan = Lan::new();
    let rotated = lan.spawn(Spec::new("rotated", 1).token("new", Some("old")));
    let behind = lan.spawn(Spec::new("behind", 2).token("old", None));
    let done = lan.spawn(Spec::new("done", 3).token("new", None));
    run_for(secs(6)).await;
    assert_eq!(rotated.peer_names().await, ["behind", "done"]);
    assert_eq!(behind.peer_names().await, ["rotated"]);
    // `done` already retired the old token: it signs with `new`, which
    // `rotated` verifies and `behind` cannot, and it cannot verify what the
    // others sign with `old`. Hence retiring only once every box has `new`.
    assert!(done.peer_names().await.is_empty());
}

// ---------- election ----------

#[tokio::test(start_paused = true)]
async fn lone_box_leads_in_about_15s() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let (l, took) = settle(&[&a], secs(30)).await;
    assert_eq!(l, "a");
    // More than 5 s with no leader heard, then more than 10 s of an
    // election nobody answers, checked on whole-second ticks: 17 s.
    assert!(
        took >= secs(15) && took <= secs(17) + secs(1) / 2,
        "{took:?}"
    );
}

/// The box holding VMs leads whatever its tiebreak; with no state the
/// bigger box does.
#[tokio::test(start_paused = true)]
async fn best_scored_box_wins() {
    let lan = Lan::new();
    let v = vec![
        lan.spawn(Spec::new("big", 9).cpus(64)),
        lan.spawn(Spec::new("holder", 1).vms(3)),
        lan.spawn(Spec::new("plain", 5)),
    ];
    let (l, took) = settle(&all(&v), secs(20)).await;
    assert_eq!(l, "holder");
    assert!(took <= secs(8), "a full cluster agrees fast: {took:?}");

    let lan = Lan::new();
    let v = vec![
        lan.spawn(Spec::new("small", 9).cpus(4)),
        lan.spawn(Spec::new("big", 1).cpus(64)),
        lan.spawn(Spec::new("mid", 5).cpus(16)),
    ];
    assert_eq!(settle(&all(&v), secs(20)).await.0, "big");
}

/// Identical boxes started together: the UUID tiebreak picks the same one
/// whatever order they come up in.
#[tokio::test(start_paused = true)]
async fn tie_break_is_stable() {
    for order in [[1u8, 2, 3, 4], [4, 3, 2, 1], [2, 4, 1, 3], [3, 1, 4, 2]] {
        let lan = Lan::new();
        let v: Vec<SimBox> = order
            .iter()
            .map(|r| lan.spawn(Spec::new(&format!("n{r}"), *r)))
            .collect();
        assert_eq!(settle(&all(&v), secs(20)).await.0, "n4", "order {order:?}");
    }
}

/// A box that starts after a cluster has a leader follows it, even with
/// the better tiebreak: the incumbent bonus keeps the job.
#[tokio::test(start_paused = true)]
async fn late_equal_box_follows_the_incumbent() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2));
    settle(&[&a, &b], secs(20)).await;
    let late = lan.spawn(Spec::new("late", 9));
    let (l, _) = settle(&[&a, &b, &late], secs(10)).await;
    assert_eq!(l, "b");
    let w = watch(&[&a, &b, &late], secs(60)).await;
    assert_eq!(w.len(), 1, "leadership moved: {w:?}");
}

/// A box with the cluster's state that comes up next to an empty leader
/// takes over (state outranks the incumbent bonus), and promptly: it
/// proposes itself as soon as it hears the weaker leader.
#[tokio::test(start_paused = true)]
async fn stronger_box_takes_over_promptly() {
    let lan = Lan::new();
    let empty = lan.spawn(Spec::new("empty", 9));
    settle(&[&empty], secs(20)).await;
    let holder = lan.spawn(Spec::new("holder", 1).vms(2));
    let t0 = lan.now();
    let w = watch(&[&empty, &holder], secs(20)).await;
    let (l, _) = settle(&[&empty, &holder], secs(1)).await;
    assert_eq!(l, "holder");
    let took = w
        .iter()
        .find(|(_, ls)| ls.as_slice() == ["holder"])
        .map(|(t, _)| *t)
        .unwrap();
    assert!(
        took <= secs(4),
        "holder led only {took:?} after joining (from {t0:?}): {w:?}"
    );
}

/// Leader dies; the rest elect a new one within the documented ~15 s.
#[tokio::test(start_paused = true)]
async fn reelection_after_leader_goes_silent() {
    for n in [2usize, 3, 5] {
        let lan = Lan::new();
        let v: Vec<SimBox> = (1..=n as u8)
            .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
            .collect();
        let (l, _) = settle(&all(&v), secs(20)).await;
        assert_eq!(l, format!("n{n}"));
        run_for(secs(3)).await;
        v[n - 1].kill();
        lan.unplug(&v[n - 1]);
        let rest: Vec<&SimBox> = v[..n - 1].iter().collect();
        let (l, took) = settle(&rest, secs(30)).await;
        assert_eq!(l, format!("n{}", n - 1), "{n} boxes");
        // ~15 s (docs): heartbeat timeout plus, for two boxes where no
        // majority is possible, the election timeout; ticks add up to 2 s.
        assert!(took <= secs(17), "{n} boxes: re-elected after {took:?}");
        if n >= 3 {
            // A majority is possible: the next best wins as soon as the
            // heartbeat timeout fires and the votes are in.
            assert!(took <= secs(8), "{n} boxes: re-elected after {took:?}");
        }
        // Followers point at the new leader, not the dead one.
        for b in &rest {
            assert_eq!(b.leader_url().await.unwrap(), v[n - 2].api_url);
        }
    }
}

/// Boxes that come up together never end up with two leaders once the
/// first election is over, and never more than briefly during it.
#[tokio::test(start_paused = true)]
async fn no_split_brain_when_started_together() {
    let lan = Lan::new();
    let v: Vec<SimBox> = [3u8, 5, 1, 4, 2]
        .iter()
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), *r)))
        .collect();
    let w = watch(&all(&v), secs(90)).await;
    let dual: Vec<_> = w.iter().filter(|(_, l)| l.len() > 1).collect();
    assert!(dual.is_empty(), "two leaders at once: {w:?}");
    assert_eq!(w.last().unwrap().1, ["n5"], "{w:?}");
    assert_eq!(
        w.iter().filter(|(_, l)| !l.is_empty()).count(),
        1,
        "leadership changed hands: {w:?}"
    );
}

/// During a partition each side may lead itself (there is no quorum
/// rule); once it heals there is one leader again, quickly, and it stays.
#[tokio::test(start_paused = true)]
async fn partition_heals_to_one_leader() {
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=4u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    assert_eq!(settle(&all(&v), secs(20)).await.0, "n4");
    lan.partition(&[&[&v[3], &v[0]], &[&v[1], &v[2]]]);
    // The leaderless side elects within ~15 s; peers on the far side go
    // stale after 15 s (swept within 10 more).
    let (l, _) = settle(&[&v[1], &v[2]], secs(30)).await;
    assert_eq!(l, "n3");
    assert!(v[3].is_leader().await);
    run_for(secs(30)).await;
    assert_eq!(v[2].peer_names().await, ["n2"]);
    lan.heal();
    let (l, took) = settle(&all(&v), secs(30)).await;
    assert!(took <= secs(6), "healed after {took:?}");
    assert!(l == "n4" || l == "n3", "{l}");
    let w = watch(&all(&v), secs(120)).await;
    assert_eq!(w.len(), 1, "flapped after healing: {w:?}");
}

/// The old leader comes back after a new one was elected: at most one
/// handover, then nothing moves.
#[tokio::test(start_paused = true)]
async fn returning_old_leader_does_not_flap() {
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=3u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    assert_eq!(settle(&all(&v), secs(20)).await.0, "n3");
    // Cable pulled: n3 still believes it leads.
    lan.unplug(&v[2]);
    assert_eq!(settle(&[&v[0], &v[1]], secs(30)).await.0, "n2");
    run_for(secs(30)).await;
    lan.plug(&v[2]);
    let w = watch(&all(&v), secs(120)).await;
    let changes: Vec<_> = w.iter().map(|(_, l)| l.clone()).collect();
    assert!(changes.len() <= 2, "flapping: {w:?}");
    assert!(settled(&all(&v)).await.is_some(), "{w:?}");
}

/// The old leader's daemon restarts (new node id, no incumbent bonus)
/// next to the new leader: equal boxes, so the incumbent keeps the job.
#[tokio::test(start_paused = true)]
async fn restarted_old_leader_does_not_take_back() {
    let lan = Lan::new();
    let specs: Vec<Spec> = (1..=3u8).map(|r| Spec::new(&format!("n{r}"), r)).collect();
    let mut v: Vec<SimBox> = specs.iter().cloned().map(|s| lan.spawn(s)).collect();
    assert_eq!(settle(&all(&v), secs(20)).await.0, "n3");
    v[2].kill();
    lan.unplug(&v[2]);
    assert_eq!(settle(&[&v[0], &v[1]], secs(30)).await.0, "n2");
    v.push(lan.spawn(specs[2].clone().restarted()));
    let live = [&v[0], &v[1], &v[3]];
    let w = watch(&live, secs(90)).await;
    assert_eq!(w.len(), 1, "leadership moved: {w:?}");
    assert_eq!(settled(&live).await.as_deref(), Some("n2"));
}

/// Every follower's forwarding target is the leader's dialable URL, and
/// nobody forwards during an election.
#[tokio::test(start_paused = true)]
async fn followers_forward_to_the_leader() {
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=3u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    // Before anyone is elected there is nowhere to forward.
    run_for(secs(2)).await;
    for b in &v {
        assert_eq!(b.leader_url().await, None, "{}", b.name);
        assert_eq!(b.leadership.status().await.state, "follower");
    }
    settle(&all(&v), secs(20)).await;
    for b in &v {
        let s = b.leadership.status().await;
        assert_eq!(s.leader.unwrap().api_url, v[2].api_url, "{}", b.name);
        assert_eq!(s.peers, 2);
        assert_eq!(s.is_leader, b.name == "n3");
    }
    // Stays pointed at the leader through announces and heartbeats.
    for _ in 0..200 {
        run_for(Duration::from_millis(100)).await;
        for b in &v[..2] {
            assert_eq!(b.leader_url().await.as_deref(), Some(v[2].api_url.as_str()));
        }
    }
}

/// A follower's peer table keeps the leader marked through the leader's
/// announces, not only between them: `Leadership::leader` falls back to it
/// whenever the follower has no vote on record.
#[tokio::test(start_paused = true)]
async fn announces_keep_the_leader_marked() {
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=2u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    settle(&all(&v), secs(20)).await;
    let mut unmarked = vec![];
    for _ in 0..300 {
        run_for(Duration::from_millis(100)).await;
        if v[0].engine.peers().read().await.current_leader().is_none() {
            unmarked.push(lan.now());
        }
    }
    assert!(unmarked.is_empty(), "leader unmarked at {unmarked:?}");
}

/// Many clusters, seeded: random sizes, start times spread over a few
/// seconds, random state. Each must settle on its best box, stay there, and
/// never show two leaders for longer than a heartbeat.
#[tokio::test(start_paused = true)]
async fn randomized_clusters_converge() {
    use rand::{Rng, SeedableRng, rngs::StdRng};
    for seed in 0..40u64 {
        let mut rng = StdRng::seed_from_u64(seed);
        let lan = Lan::new();
        let n = rng.gen_range(2..=6u8);
        let mut v = vec![];
        let mut best: Option<(usize, u8, String)> = None;
        for r in 1..=n {
            run_for(Duration::from_millis(rng.gen_range(0..1500))).await;
            let vms = if rng.gen_bool(0.3) {
                rng.gen_range(1..4)
            } else {
                0
            };
            let name = format!("n{r}");
            if best
                .as_ref()
                .is_none_or(|(bv, br, _)| (vms, r) > (*bv, *br))
            {
                best = Some((vms, r, name.clone()));
            }
            v.push(lan.spawn(Spec::new(&name, r).vms(vms)));
        }
        let boxes = all(&v);
        let w = watch(&boxes, secs(30)).await;
        let want = best.unwrap().2;
        assert_eq!(
            settled(&boxes).await.as_deref(),
            Some(want.as_str()),
            "seed {seed}: {w:?}"
        );
        // Longest stretch with two leaders.
        let mut worst = Duration::ZERO;
        for (i, (t, l)) in w.iter().enumerate() {
            if l.len() > 1 {
                let end = w.get(i + 1).map(|(t2, _)| *t2).unwrap_or(secs(30));
                worst = worst.max(end - *t);
            }
        }
        assert!(
            worst <= secs(1),
            "seed {seed}: two leaders for {worst:?}: {w:?}"
        );
        let after = watch(&boxes, secs(30)).await;
        assert_eq!(
            after.len(),
            1,
            "seed {seed}: moved after settling: {after:?}"
        );
    }
}

// ---------- discovery off, and switching it ----------

/// Discovery off (the default): the box leads alone at once and puts
/// nothing on the wire.
#[tokio::test(start_paused = true)]
async fn discovery_off_leads_alone_and_stays_silent() {
    let lan = Lan::new();
    let cluster = lan.spawn(Spec::new("cluster", 1));
    let off = lan.spawn(Spec::new("off", 9).disabled());
    run_for(secs(2)).await;
    assert!(off.is_leader().await);
    assert_eq!(off.leader_url().await.unwrap(), off.api_url);
    run_for(secs(30)).await;
    assert!(off.peer_names().await.is_empty());
    assert!(
        sent_by(&lan, &off).is_empty(),
        "a box with discovery off sent {:?}",
        sent_by(&lan, &off)
            .iter()
            .map(|(.., m)| m.clone())
            .collect::<Vec<_>>()
    );
    assert!(cluster.peer_names().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn switching_discovery_on_joins_and_off_leaves() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let b = lan.spawn(Spec::new("b", 2));
    let c = lan.spawn(Spec::new("c", 3).disabled());
    assert_eq!(settle(&[&a, &b], secs(20)).await.0, "b");
    assert!(c.is_leader().await);

    // On: c is heard within an announce interval and the three agree on
    // one leader.
    c.engine.set_enabled(true);
    run_for(secs(6)).await;
    assert_eq!(a.peer_names().await, ["b", "c"]);
    assert_eq!(c.peer_names().await, ["a", "b"]);
    let (l, _) = settle(&[&a, &b, &c], secs(15)).await;
    let w = watch(&[&a, &b, &c], secs(60)).await;
    assert_eq!(w.len(), 1, "{w:?}");

    // Off: c leads alone within a second and forgets its peers; the others
    // drop it once it goes stale and re-elect if it was theirs.
    c.engine.set_enabled(false);
    run_for(secs(2)).await;
    assert!(c.is_leader().await);
    assert!(c.peer_names().await.is_empty());
    let (l2, took) = settle(&[&a, &b], secs(30)).await;
    if l == "c" {
        assert!(took <= secs(16), "{took:?}");
    } else {
        assert_eq!(l2, l);
    }
    run_for(secs(25)).await;
    assert_eq!(a.peer_names().await, ["b"]);
}

/// `PUT /v1/settings {"discovery": …}` reaches the running engine.
#[tokio::test(start_paused = true)]
async fn settings_switch_drives_the_engine() {
    use crate::settings::{Change, Settings, SettingsCtl};
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    let off = lan.spawn(Spec::new("off", 2).disabled());
    let ctl = SettingsCtl::in_memory(Default::default(), Settings::default())
        .with_engine(off.engine.clone());
    assert!(!off.engine.enabled());
    let s = ctl
        .set(Change {
            mesh_routed: None,
            discovery: Some(true),
        })
        .await
        .unwrap();
    assert!(s.discovery && off.engine.enabled());
    let (l, _) = settle(&[&a, &off], secs(20)).await;
    assert_eq!(l, "off", "it led alone, so it keeps the incumbent bonus");
    ctl.set(Change {
        mesh_routed: None,
        discovery: Some(false),
    })
    .await
    .unwrap();
    assert!(!off.engine.enabled());
    assert!(!ctl.get().await.discovery);
    run_for(secs(2)).await;
    assert!(off.peer_names().await.is_empty());
}

/// Bootstrap's 20 s listen, fed what a running simulated cluster actually
/// put on the wire: it hears every box, marks the leader, and (without a
/// terminal) refuses to found a second cluster.
#[tokio::test(start_paused = true)]
async fn bootstrap_hears_a_running_cluster() {
    use crate::bootstrap::discover::{
        DEFAULT_LISTEN_SECS, decide, listen_on,
        tests::{Air, FakeClock},
    };
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=3u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    settle(&all(&v), secs(20)).await;
    let t0 = lan.now();
    run_for(secs(DEFAULT_LISTEN_SECS)).await;
    let frames: Vec<_> = lan
        .log()
        .into_iter()
        .filter(|f| f.at >= t0)
        .map(|f| (f.at - t0, f.bytes, f.src))
        .collect();
    let clock = FakeClock::default();
    let air = Air::new(&clock, frames);
    let found = listen_on(
        &air,
        &clock,
        |_| false,
        secs(DEFAULT_LISTEN_SECS),
        "new-box",
    )
    .unwrap();
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].hostname, "n3");
    assert!(found[0].is_leader);
    assert_eq!(
        found[0].api_url, v[2].api_url,
        "dialable, not the .invalid name"
    );
    assert!(found[1..].iter().all(|f| !f.is_leader));
    let e = decide("239.0.0.42:23442", &found, false, |_: &str| unreachable!())
        .unwrap_err()
        .to_string();
    assert!(e.contains(&format!("--join {}", v[2].api_url)), "{e}");

    // A silent LAN: the full 20 s, nothing heard, found a cluster.
    let clock = FakeClock::default();
    let air = Air::new(&clock, vec![]);
    let found = listen_on(
        &air,
        &clock,
        |_| false,
        secs(DEFAULT_LISTEN_SECS),
        "new-box",
    )
    .unwrap();
    assert!(found.is_empty());
    assert_eq!(
        decide("239.0.0.42:23442", &found, false, |_: &str| unreachable!()).unwrap(),
        crate::bootstrap::discover::Choice::First
    );
}

/// Several followers' votes for the same proposer arrive twice (two
/// interfaces on one segment, say): duplicates count once.
#[tokio::test(start_paused = true)]
async fn duplicated_datagrams_are_harmless() {
    let lan = Lan::new();
    let v: Vec<SimBox> = (1..=3u8)
        .map(|r| lan.spawn(Spec::new(&format!("n{r}"), r)))
        .collect();
    // Echo every frame a second time for the first 30 s.
    let mut seen = 0;
    let t0 = lan.now();
    while lan.now() - t0 < secs(30) {
        let log = lan.log();
        for f in &log[seen..] {
            for b in &v {
                if b.idx != f.from {
                    lan.inject(b, f.bytes.clone(), f.src);
                }
            }
        }
        seen = log.len();
        run_for(Duration::from_millis(100)).await;
    }
    assert_eq!(settled(&all(&v)).await.as_deref(), Some("n3"));
    for b in &v {
        assert_eq!(b.peer_names().await.len(), 2);
    }
}

/// The election state a box reports matches its engine's view.
#[tokio::test(start_paused = true)]
async fn status_reports_election_state() {
    let lan = Lan::new();
    let a = lan.spawn(Spec::new("a", 1));
    run_for(secs(1)).await;
    assert_eq!(a.state().await, ElectionState::Follower);
    run_for(Duration::from_millis(5500)).await;
    assert_eq!(a.state().await, ElectionState::Election);
    assert_eq!(a.leadership.status().await.state, "election");
    assert_eq!(a.leadership.leader().await, None);
    run_for(secs(11)).await;
    assert_eq!(a.leadership.status().await.state, "leader");
}

/// Real multicast smoke test: two engines on a private group elect one
/// leader. Needs multicast loopback and 23442/udp open.
/// `XDEBBIE_TEST_MULTICAST=1 cargo test -p xdebbied -- --ignored multicast`
#[tokio::test]
#[ignore = "real multicast; XDEBBIE_TEST_MULTICAST=1"]
async fn multicast_two_engines_elect_one_leader() {
    use super::engine::{DiscoveryConfig, DiscoveryEngine};
    use std::sync::Arc;
    if std::env::var_os("XDEBBIE_TEST_MULTICAST").is_none() {
        eprintln!("XDEBBIE_TEST_MULTICAST unset; skipping");
        return;
    }
    let tokens = || {
        Some(Arc::new(crate::token::Tokens::in_memory(
            xdebbie_core::TokenSet {
                current: "smoke".into(),
                previous: None,
            },
        )))
    };
    let mk = |name: &str| {
        let e = Arc::new(
            DiscoveryEngine::new(DiscoveryConfig {
                multicast_addr: "239.0.0.44:23442".into(),
                hostname: name.into(),
                tokens: tokens(),
                ..Default::default()
            })
            .unwrap(),
        );
        tokio::spawn(e.clone().run());
        e
    };
    let (a, b) = (mk("smoke-a"), mk("smoke-b"));
    let t0 = std::time::Instant::now();
    loop {
        let la = a.election().read().await.is_leader();
        let lb = b.election().read().await.is_leader();
        let pa = a.peers().read().await.peers().count();
        let pb = b.peers().read().await.peers().count();
        if la != lb && pa == 1 && pb == 1 {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(40),
            "no single leader: a {la} ({pa} peers), b {lb} ({pb} peers)"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
