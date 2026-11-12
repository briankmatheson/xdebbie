// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Multicast discovery message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiscoveryMessage {
    /// Announce presence on the network
    Announce {
        node_id: Uuid,
        hostname: String,
        api_url: String,
        capacity: NodeCapacity,
        score: f64,
        sequence: u64,
    },
    /// Periodic heartbeat
    Heartbeat {
        node_id: Uuid,
        score: f64,
        is_leader: bool,
        sequence: u64,
    },
    /// Propose self as leader
    ElectionPropose {
        node_id: Uuid,
        score: f64,
        sequence: u64,
    },
    /// Vote for a leader candidate
    ElectionVote {
        node_id: Uuid,
        voted_for: Uuid,
        sequence: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapacity {
    pub total_cpus: u32,
    pub total_memory_mib: u64,
    pub available_cpus: f32,
    pub available_memory_mib: u64,
    pub vm_count: usize,
    pub control_plane_eligible: bool,
}

impl DiscoveryMessage {
    pub fn node_id(&self) -> Uuid {
        match self {
            Self::Announce { node_id, .. }
            | Self::Heartbeat { node_id, .. }
            | Self::ElectionPropose { node_id, .. }
            | Self::ElectionVote { node_id, .. } => *node_id,
        }
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}
