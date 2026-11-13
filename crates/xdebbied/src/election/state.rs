// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::{Duration, Instant};
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

            // Step down if we're leader
            if self.state == ElectionState::Leader {
                self.step_down();
            }

            true // Vote for them
        } else {
            // We have higher score, reject their proposal and start our own election
            tracing::debug!(
                "Rejecting election proposal from {} (score {:.1} < {:.1}), starting own election",
                proposer_id,
                proposer_score,
                self.score
            );

            if self.state != ElectionState::Election && self.state != ElectionState::Leader {
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
}
