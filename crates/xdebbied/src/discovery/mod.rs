// SPDX-License-Identifier: GPL-3.0-or-later
//! Multicast peer discovery for XDebbie
//!
//! This module implements automatic peer discovery using multicast UDP.
//! Nodes announce themselves on 239.0.0.42:7442 with ANNOUNCE and HEARTBEAT messages.

pub mod engine;
pub mod messages;
pub mod multicast;
pub mod peer;
pub mod reach;

pub use engine::{DiscoveryConfig, DiscoveryEngine};
