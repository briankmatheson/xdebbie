// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;
// tokio's clock: the real one in the daemon, a paused, hand-advanced one in
// the simulated-LAN tests (discovery/sim.rs).
use tokio::time::Instant;
use uuid::Uuid;

/// Election state for the Bully algorithm
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElectionState {
    /// Not participating in election (just started)
    Inactive,
    /// Normal operation, not leader
    Follower,
    /// Election in progress
    Election,
    /// This node is the leader
    Leader,
}

impl std::fmt::Display for ElectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inactive => write!(f, "INACTIVE"),
            Self::Follower => write!(f, "FOLLOWER"),
            Self::Election => write!(f, "ELECTION"),
            Self::Leader => write!(f, "LEADER"),
        }
    }
}

/// Election state machine
pub struct ElectionStateMachine {
    /// Current state
    state: ElectionState,
    /// Our node ID
    node_id: Uuid,
    /// Our current score
    score: f64,
    /// Time we entered current state
    state_entered: Instant,
    /// Last time we received a heartbeat from a leader
    last_leader_heartbeat: Option<Instant>,
    /// Current leader (if known)
    current_leader: Option<Uuid>,
    /// Election timeout
    election_timeout: Duration,
    /// Heartbeat timeout
    heartbeat_timeout: Duration,
    /// Votes received in current election
    votes_received: std::collections::HashSet<Uuid>,
    /// Sequence number for messages
    sequence: u64,
}

impl ElectionStateMachine {
    pub fn new(node_id: Uuid, election_timeout: Duration, heartbeat_timeout: Duration) -> Self {
        Self {
            state: ElectionState::Inactive,
            node_id,
            score: 0.0,
            state_entered: Instant::now(),
            last_leader_heartbeat: None,
            current_leader: None,
            election_timeout,
            heartbeat_timeout,
            votes_received: std::collections::HashSet::new(),
            sequence: 0,
        }
    }

    pub fn state(&self) -> ElectionState {
        self.state
    }

    pub fn is_leader(&self) -> bool {
        self.state == ElectionState::Leader
    }

    pub fn current_leader(&self) -> Option<Uuid> {
        self.current_leader
    }

    pub fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    pub fn update_score(&mut self, score: f64) {
        self.score = score;
    }

    /// Transition to a new state
    fn transition(&mut self, new_state: ElectionState) {
        if self.state != new_state {
            tracing::info!("Election state transition: {} -> {}", self.state, new_state);
            self.state = new_state;
            self.state_entered = Instant::now();

            if new_state == ElectionState::Election {
                self.votes_received.clear();
                self.votes_received.insert(self.node_id); // Vote for self
            }
        }
    }

    /// Start from inactive to follower
    pub fn activate(&mut self) {
        self.transition(ElectionState::Follower);
    }

    /// Start an election
    pub fn start_election(&mut self) {
        self.transition(ElectionState::Election);
    }

    /// Become leader
    pub fn become_leader(&mut self) {
        self.current_leader = Some(self.node_id);
        self.transition(ElectionState::Leader);
    }

    /// Step down from leader to follower
    pub fn step_down(&mut self) {
        if self.state == ElectionState::Leader {
            self.current_leader = None;
            self.transition(ElectionState::Follower);
        }
    }

    /// Received a heartbeat from a leader
    pub fn received_leader_heartbeat(&mut self, leader_id: Uuid, leader_score: f64) {
        self.last_leader_heartbeat = Some(Instant::now());

        // If we see a higher-scored leader, follow them
        if leader_score > self.score || (leader_score == self.score && leader_id > self.node_id) {
            if self.current_leader != Some(leader_id) {
                tracing::info!(
                    "Recognized new leader: {} (score: {:.1})",
                    leader_id,
                    leader_score
                );
                self.current_leader = Some(leader_id);
            }

            if self.state != ElectionState::Follower {
                self.transition(ElectionState::Follower);
            }
        }
        // If we have higher score, challenge them
        else if self.state != ElectionState::Leader && self.state != ElectionState::Election {
            tracing::info!(
                "Received heartbeat from lower-scored leader {} ({:.1} < {:.1}), starting election",
                leader_id,
                leader_score,
                self.score
            );
            self.start_election();
        }
    }

    /// Received an election proposal from another node
    pub fn received_election_propose(&mut self, proposer_id: Uuid, proposer_score: f64) -> bool {
        // If proposer has higher score, vote for them
        if proposer_score > self.score
            || (proposer_score == self.score && proposer_id > self.node_id)
        {
            tracing::debug!(
                "Voting for {} in election (score {:.1} > {:.1})",
                proposer_id,
                proposer_score,
                self.score
            );

            // Step down if we're leader; stop campaigning if we are a
            // candidate. Otherwise votes from boxes weaker than both of us
            // can give us a majority too, and two leaders come out of one
            // election.
            if self.state == ElectionState::Leader {
                self.step_down();
            } else if self.state == ElectionState::Election {
                self.transition(ElectionState::Follower);
            }
            // Give the better candidate a heartbeat timeout to win and say
            // so before we call another election.
            self.last_leader_heartbeat = Some(Instant::now());

            true // Vote for them
        } else {
            // We have the higher score: no vote. Run ourselves unless a
            // leader or a better candidate has been heard within the
            // heartbeat timeout; then the proposer will hear from them, and
            // a campaign of ours would only split the votes (a box needs
            // only to outrank half the cluster to collect a majority).
            let quiet = !self.heard_leader_recently();
            tracing::debug!(
                "Rejecting election proposal from {} (score {:.1} < {:.1}){}",
                proposer_id,
                proposer_score,
                self.score,
                if quiet { ", starting own election" } else { "" }
            );

            if self.state == ElectionState::Follower && quiet {
                self.start_election();
            }

            false // Don't vote for them
        }
    }

    /// Received a vote in current election
    pub fn received_vote(&mut self, voter_id: Uuid) {
        if self.state == ElectionState::Election {
            self.votes_received.insert(voter_id);
            tracing::debug!(
                "Received vote from {} ({} total votes)",
                voter_id,
                self.votes_received.len()
            );
        }
    }

    /// Check if we have enough votes to become leader (simple majority)
    pub fn has_majority(&self, total_nodes: usize) -> bool {
        if self.state != ElectionState::Election {
            return false;
        }

        let required_votes = (total_nodes / 2) + 1;
        self.votes_received.len() >= required_votes
    }

    /// Has a leader (or a better candidate we voted for) been heard within
    /// the heartbeat timeout?
    pub fn heard_leader_recently(&self) -> bool {
        self.last_leader_heartbeat
            .is_some_and(|t| t.elapsed() <= self.heartbeat_timeout)
    }

    /// Our score and id, as peers compare them.
    pub fn outranked_by(&self, score: f64, id: Uuid) -> bool {
        score > self.score || (score == self.score && id > self.node_id)
    }

    /// Check if leader heartbeat has timed out
    pub fn leader_heartbeat_timeout(&self) -> bool {
        if let Some(last_heartbeat) = self.last_leader_heartbeat {
            last_heartbeat.elapsed() > self.heartbeat_timeout
        } else {
            // No leader seen yet, should start election after activation
            self.state != ElectionState::Inactive
                && self.state_entered.elapsed() > self.heartbeat_timeout
        }
    }

    /// Check if election has timed out (no winner emerged)
    pub fn election_timeout(&self) -> bool {
        self.state == ElectionState::Election
            && self.state_entered.elapsed() > self.election_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_transitions() {
        let node_id = Uuid::new_v4();
        let mut sm =
            ElectionStateMachine::new(node_id, Duration::from_secs(5), Duration::from_secs(3));

        assert_eq!(sm.state(), ElectionState::Inactive);

        sm.activate();
        assert_eq!(sm.state(), ElectionState::Follower);

        sm.start_election();
        assert_eq!(sm.state(), ElectionState::Election);

        sm.become_leader();
        assert_eq!(sm.state(), ElectionState::Leader);
        assert!(sm.is_leader());

        sm.step_down();
        assert_eq!(sm.state(), ElectionState::Follower);
        assert!(!sm.is_leader());
    }

    #[test]
    fn test_voting() {
        let node_id = Uuid::new_v4();
        let mut sm =
            ElectionStateMachine::new(node_id, Duration::from_secs(5), Duration::from_secs(3));

        sm.start_election();

        // Self vote
        assert_eq!(sm.votes_received.len(), 1);

        // Add more votes
        sm.received_vote(Uuid::new_v4());
        sm.received_vote(Uuid::new_v4());

        assert_eq!(sm.votes_received.len(), 3);
        assert!(sm.has_majority(5)); // 3 out of 5 is majority
        assert!(!sm.has_majority(10)); // 3 out of 10 is not majority
    }

    fn sm(rank: u8) -> ElectionStateMachine {
        let mut b = [0u8; 16];
        b[0] = rank;
        let mut sm = ElectionStateMachine::new(
            Uuid::from_bytes(b),
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        sm.update_score(100.0);
        sm.activate();
        sm
    }

    fn id(rank: u8) -> Uuid {
        let mut b = [0u8; 16];
        b[0] = rank;
        Uuid::from_bytes(b)
    }

    #[tokio::test(start_paused = true)]
    async fn equal_scores_break_ties_by_node_id() {
        let mut lo = sm(1);
        assert!(lo.received_election_propose(id(2), 100.0), "votes up");
        let mut hi = sm(3);
        assert!(!hi.received_election_propose(id(2), 100.0), "not down");
        assert!(hi.outranked_by(100.0, id(4)));
        assert!(!hi.outranked_by(100.0, id(2)));
        assert!(hi.outranked_by(100.5, id(1)));
    }

    #[tokio::test(start_paused = true)]
    async fn candidate_stands_down_for_a_better_one() {
        let mut c = sm(2);
        c.start_election();
        c.received_vote(id(1));
        assert!(c.received_election_propose(id(3), 200.0));
        assert_eq!(c.state(), ElectionState::Follower);
        // Votes that arrive late count for nothing now.
        c.received_vote(id(1));
        assert!(!c.has_majority(3));
        // And it gives the better candidate a heartbeat timeout.
        assert!(c.heard_leader_recently());
        assert!(!c.leader_heartbeat_timeout());
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(c.leader_heartbeat_timeout());
    }

    #[tokio::test(start_paused = true)]
    async fn leader_steps_down_for_a_better_proposal_or_leader() {
        let mut l = sm(2);
        l.become_leader();
        assert!(l.received_election_propose(id(3), 1e6));
        assert_eq!(l.state(), ElectionState::Follower);

        let mut l = sm(2);
        l.become_leader();
        l.received_leader_heartbeat(id(3), 1e6);
        assert_eq!(l.state(), ElectionState::Follower);
        assert_eq!(l.current_leader(), Some(id(3)));

        // A weaker leader is ignored by a leader...
        let mut l = sm(2);
        l.become_leader();
        l.received_leader_heartbeat(id(1), 1.0);
        assert!(l.is_leader());
        // ...and challenged by a follower.
        let mut f = sm(2);
        f.received_leader_heartbeat(id(1), 1.0);
        assert_eq!(f.state(), ElectionState::Election);
    }

    #[tokio::test(start_paused = true)]
    async fn weaker_proposal_starts_a_campaign_only_without_a_leader() {
        // Nobody heard: the better box runs.
        let mut f = sm(2);
        assert!(!f.received_election_propose(id(1), 1.0));
        assert_eq!(f.state(), ElectionState::Election);
        // A live leader: no campaign; the proposer will hear the leader.
        let mut f = sm(2);
        f.received_leader_heartbeat(id(3), 1e6);
        assert!(!f.received_election_propose(id(1), 1.0));
        assert_eq!(f.state(), ElectionState::Follower);
        // That leader went quiet: run.
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(!f.received_election_propose(id(1), 1.0));
        assert_eq!(f.state(), ElectionState::Election);
    }

    #[tokio::test(start_paused = true)]
    async fn timeouts() {
        let mut f = sm(1);
        assert!(!f.leader_heartbeat_timeout());
        tokio::time::advance(Duration::from_millis(5001)).await;
        assert!(f.leader_heartbeat_timeout(), "no leader since activation");
        f.start_election();
        assert!(!f.election_timeout());
        tokio::time::advance(Duration::from_millis(10001)).await;
        assert!(f.election_timeout());
        // An inactive machine never times out into an election.
        let i = ElectionStateMachine::new(id(1), Duration::from_secs(1), Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(!i.leader_heartbeat_timeout());
    }

    #[test]
    fn majority_needs_an_election_and_half_plus_one() {
        let mut c = sm(1);
        assert!(!c.has_majority(1), "not campaigning");
        c.start_election();
        assert!(c.has_majority(1), "alone");
        assert!(!c.has_majority(2));
        c.received_vote(id(5));
        c.received_vote(id(5));
        assert!(c.has_majority(2) && c.has_majority(3) && !c.has_majority(4));
        c.become_leader();
        assert!(!c.has_majority(1));
    }

    #[test]
    fn sequence_rises() {
        let mut c = sm(1);
        let a = c.next_sequence();
        assert!(c.next_sequence() > a);
    }
}
