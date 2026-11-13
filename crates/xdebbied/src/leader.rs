// SPDX-License-Identifier: GPL-3.0-or-later
//! Who decides. One xdebbied per cluster is the leader (docs/autodiscovery.md):
//! it runs the reconciler and owns every `/v1` read and write. Followers
//! forward those requests to it, so a client may point at any box and
//! `xdctl` moves itself to the leader after one round trip. A box without
//! discovery (disabled, or no multicast) is its own leader.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::RwLock;
use xdebbie_core::{LeaderRef, LeaderStatus};

use crate::{
    discovery::{DiscoveryEngine, peer::PeerTable},
    election::state::{ElectionState, ElectionStateMachine},
};

#[derive(Clone)]
pub struct Leadership {
    me: LeaderRef,
    cluster: Option<Cluster>,
}

#[derive(Clone)]
struct Cluster {
    election: Arc<RwLock<ElectionStateMachine>>,
    peers: Arc<RwLock<PeerTable>>,
}

impl Leadership {
    /// No discovery: this box decides.
    pub fn standalone(hostname: impl Into<String>, api_url: impl Into<String>) -> Self {
        Self {
            me: LeaderRef {
                node_id: None,
                hostname: hostname.into(),
                api_url: api_url.into(),
            },
            cluster: None,
        }
    }

    /// Follow the election the discovery engine runs.
    pub fn elected(
        engine: &DiscoveryEngine,
        hostname: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Self {
        Self {
            me: LeaderRef {
                node_id: Some(engine.node_id()),
                hostname: hostname.into(),
                api_url: api_url.into(),
            },
            cluster: Some(Cluster {
                election: engine.election(),
                peers: engine.peers(),
            }),
        }
    }

    /// A box in a cluster whose election state the test drives directly.
    #[cfg(test)]
    pub fn for_test(
        me: LeaderRef,
        election: Arc<RwLock<ElectionStateMachine>>,
        peers: Arc<RwLock<PeerTable>>,
    ) -> Self {
        Self {
            me,
            cluster: Some(Cluster { election, peers }),
        }
    }

    pub fn is_standalone(&self) -> bool {
        self.cluster.is_none()
    }

    pub async fn is_leader(&self) -> bool {
        match &self.cluster {
            None => true,
            Some(c) => c.election.read().await.is_leader(),
        }
    }

    /// The box that decides right now; `None` while an election is running.
    pub async fn leader(&self) -> Option<LeaderRef> {
        let Some(c) = &self.cluster else {
            return Some(self.me.clone());
        };
        let (state, voted) = {
            let e = c.election.read().await;
            (e.state(), e.current_leader())
        };
        match state {
            ElectionState::Leader => return Some(self.me.clone()),
            ElectionState::Follower => {}
            // Mid-election (or not started): nobody decides yet.
            ElectionState::Election | ElectionState::Inactive => return None,
        }
        let peers = c.peers.read().await;
        // Whoever the election settled on, else whoever is heartbeating as
        // leader (covers the window before this box has voted).
        voted
            .and_then(|id| peers.get(&id))
            .or_else(|| peers.current_leader())
            .map(|p| LeaderRef {
                node_id: Some(p.node_id),
                hostname: p.hostname.clone(),
                api_url: p.api_url.clone(),
            })
    }

    /// Every peer this box currently sees (none when standalone).
    pub async fn peers(&self) -> Vec<LeaderRef> {
        let Some(c) = &self.cluster else {
            return vec![];
        };
        c.peers
            .read()
            .await
            .peers()
            .map(|p| LeaderRef {
                node_id: Some(p.node_id),
                hostname: p.hostname.clone(),
                api_url: p.api_url.clone(),
            })
            .collect()
    }

    pub async fn status(&self) -> LeaderStatus {
        let Some(c) = &self.cluster else {
            return LeaderStatus {
                this: self.me.clone(),
                state: "standalone".into(),
                is_leader: true,
                leader: Some(self.me.clone()),
                peers: 0,
            };
        };
        let state = c.election.read().await.state();
        LeaderStatus {
            this: self.me.clone(),
            state: match state {
                ElectionState::Inactive => "inactive",
                ElectionState::Follower => "follower",
                ElectionState::Election => "election",
                ElectionState::Leader => "leader",
            }
            .into(),
            is_leader: state == ElectionState::Leader,
            leader: self.leader().await,
            peers: c.peers.read().await.peers().count(),
        }
    }

    /// A flag mirroring `is_leader` for consumers that cannot await (the
    /// DHCP gate), refreshed every second. Standalone: always true.
    pub fn flag(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(self.is_standalone()));
        if let Some(c) = self.cluster.clone() {
            let f = flag.clone();
            tokio::spawn(async move {
                loop {
                    f.store(c.election.read().await.is_leader(), Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
        flag
    }
}

/// Name a box by hostname, falling back to `unknown` like discovery does.
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn standalone_is_leader() {
        let l = Leadership::standalone("box", "http://10.0.0.1:7443");
        assert!(l.is_leader().await);
        let s = l.status().await;
        assert_eq!(s.state, "standalone");
        assert_eq!(s.leader.unwrap().api_url, "http://10.0.0.1:7443");
        assert!(l.flag().load(Ordering::Relaxed));
    }

    fn cluster(me: Uuid) -> (Leadership, Cluster) {
        let c = Cluster {
            election: Arc::new(RwLock::new(ElectionStateMachine::new(
                me,
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(5),
            ))),
            peers: Arc::new(RwLock::new(PeerTable::new())),
        };
        let me = LeaderRef {
            node_id: Some(me),
            hostname: "me".into(),
            api_url: "http://me:7443".into(),
        };
        (
            Leadership::for_test(me, c.election.clone(), c.peers.clone()),
            c,
        )
    }

    #[tokio::test]
    async fn follows_the_elected_peer() {
        use crate::discovery::peer::PeerInfo;
        let me = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (l, c) = cluster(me);
        // Nothing elected yet: no leader, not us.
        c.election.write().await.activate();
        assert!(!l.is_leader().await);
        assert_eq!(l.leader().await, None);
        assert_eq!(l.status().await.state, "follower");

        // A peer heartbeats as leader with a higher score: follow it.
        c.peers.write().await.insert(PeerInfo::new(
            other,
            "other".into(),
            "http://other:7443".into(),
            10.0,
        ));
        c.peers
            .write()
            .await
            .get_mut(&other)
            .unwrap()
            .update_heartbeat(10.0, true, 1);
        c.election
            .write()
            .await
            .received_leader_heartbeat(other, 1e9);
        let lead = l.leader().await.unwrap();
        assert_eq!(lead.api_url, "http://other:7443");
        assert_eq!(lead.node_id, Some(other));
        assert!(!l.is_leader().await);

        // We win: we are the leader.
        c.election.write().await.become_leader();
        assert!(l.is_leader().await);
        assert_eq!(l.leader().await.unwrap().api_url, "http://me:7443");
        let s = l.status().await;
        assert_eq!((s.state.as_str(), s.peers), ("leader", 1));
    }
}
