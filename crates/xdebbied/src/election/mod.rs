// SPDX-License-Identifier: GPL-3.0-or-later
//! Leader election using the Bully algorithm
//!
//! This module implements leader election with score-based ranking:
//! - Uptime: Nodes that have been running longer are preferred
//! - Capacity: Total compute resources available
//! - Load: Less loaded nodes score higher
//! - Incumbent bonus: Current leader gets preference to reduce churn

pub mod scoring;
pub mod state;

pub use scoring::{calculate_score, get_uptime};
pub use state::{ElectionState, ElectionStateMachine};
