// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;
use uuid::Uuid;

use crate::discovery::messages::NodeCapacity;

/// Calculate node score for leader election using the Bully algorithm
///
/// Higher score = more likely to be leader
///
/// Score components:
/// 1. Uptime (0-1000): Nodes that have been running longer are preferred
/// 2. Capacity (0-1000): Total compute resources available
/// 3. Load (0-1000): Inverse of current utilization (less loaded = higher score)
/// 4. Was leader bonus (+500): Current leader gets preference to avoid churn
/// 5. State (0-4000): VMs in this box's store. Stores are per box
///    (docs/autodiscovery.md, "Known gap"), so the one holding the cluster's
///    VMs must outrank an empty one whatever its uptime — otherwise a leader
///    restart hands the cluster to a box that knows nothing about it.
/// 6. UUID tiebreaker: Deterministic ordering when scores are equal
pub fn calculate_score(
    capacity: &NodeCapacity,
    uptime: Duration,
    was_leader: bool,
    node_id: Uuid,
) -> f64 {
    let uptime_score = uptime_component(uptime);
    let capacity_score = capacity_component(capacity);
    let load_score = load_component(capacity);
    let leader_bonus = if was_leader { 500.0 } else { 0.0 };
    let state_score = state_component(capacity.vm_count);
    let uuid_tiebreaker = uuid_component(node_id);

    let score = uptime_score + capacity_score + load_score + leader_bonus + state_score + uuid_tiebreaker;

    tracing::debug!(
        "Score calculation: uptime={:.1} capacity={:.1} load={:.1} leader_bonus={:.1} state={:.1} uuid={:.6} total={:.1}",
        uptime_score, capacity_score, load_score, leader_bonus, state_score, uuid_tiebreaker, score
    );

    score
}

/// Uptime component: 0-1000 points based on how long the node has been running
///
/// Scoring:
/// - 0-1 hour: 0-250 points (linear)
/// - 1-24 hours: 250-750 points (linear)
/// - 24+ hours: 750-1000 points (logarithmic)
fn uptime_component(uptime: Duration) -> f64 {
    let hours = uptime.as_secs_f64() / 3600.0;

    if hours < 1.0 {
        // First hour: 0-250 points
        hours * 250.0
    } else if hours < 24.0 {
        // 1-24 hours: 250-750 points
        250.0 + (hours - 1.0) * (500.0 / 23.0)
    } else {
        // 24+ hours: 750-1000 points (logarithmic)
        let days = hours / 24.0;
        750.0 + (days.ln() * 100.0).min(250.0)
    }
}

/// Capacity component: 0-1000 points based on total resources
///
/// Combines CPU and memory capacity with equal weighting
fn capacity_component(capacity: &NodeCapacity) -> f64 {
    // Normalize CPU (assume 128 cores is "max" for scoring)
    let cpu_score = (capacity.total_cpus as f64 / 128.0).min(1.0) * 500.0;

    // Normalize memory (assume 512 GiB is "max" for scoring)
    let mem_score = (capacity.total_memory_mib as f64 / (512.0 * 1024.0)).min(1.0) * 500.0;

    cpu_score + mem_score
}

/// Load component: 0-1000 points based on current utilization
///
/// Inverse of utilization - less loaded nodes score higher
fn load_component(capacity: &NodeCapacity) -> f64 {
    // Calculate CPU utilization
    let cpu_used = capacity.total_cpus as f32 - capacity.available_cpus;
    let cpu_util = if capacity.total_cpus > 0 {
        (cpu_used / capacity.total_cpus as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Calculate memory utilization
    let mem_used = capacity.total_memory_mib - capacity.available_memory_mib;
    let mem_util = if capacity.total_memory_mib > 0 {
        (mem_used as f64 / capacity.total_memory_mib as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Average utilization
    let avg_util = (cpu_util as f64 + mem_util) / 2.0;

    // Inverse for score (less loaded = higher score)
    (1.0 - avg_util) * 1000.0
}

/// State component: 2000 for holding any VM at all, then 200 per VM up to
/// ten. The flat part outweighs the leader bonus (500) plus the whole
/// uptime range (1000), so a box that just restarted with the cluster's
/// VMs in its store still beats an empty incumbent that has been up for
/// days. The graded part orders two non-empty stores.
fn state_component(vm_count: usize) -> f64 {
    if vm_count == 0 { 0.0 } else { 2000.0 + vm_count.min(10) as f64 * 200.0 }
}

/// UUID component: 0-1 tiebreaker for deterministic ordering
///
/// Uses the first 8 bytes of the UUID as a fraction
fn uuid_component(node_id: Uuid) -> f64 {
    let bytes = node_id.as_bytes();
    let value = u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);

    value as f64 / u64::MAX as f64
}

/// Get system uptime
pub fn get_uptime() -> Duration {
    // Try to read /proc/uptime on Linux
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/uptime")
            && let Some(uptime_str) = content.split_whitespace().next()
                && let Ok(uptime_secs) = uptime_str.parse::<f64>() {
                    return Duration::from_secs_f64(uptime_secs);
                }
    }

    // Fallback: assume we just started (will be overridden by process start time tracking)
    Duration::from_secs(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uptime_scoring() {
        assert_eq!(uptime_component(Duration::from_secs(0)), 0.0);
        assert!((uptime_component(Duration::from_secs(1800)) - 125.0).abs() < 1.0); // 30 min
        assert!((uptime_component(Duration::from_secs(3600)) - 250.0).abs() < 1.0); // 1 hour
        assert!((uptime_component(Duration::from_secs(3600 * 12)) - 500.0).abs() < 50.0); // 12 hours
        assert!(uptime_component(Duration::from_secs(3600 * 24)) >= 750.0); // 24 hours
    }

    /// A freshly restarted box with the cluster's VMs in its store outranks
    /// an empty one that has been up for a day and holds the leader bonus.
    #[test]
    fn store_with_vms_beats_empty_incumbent() {
        let cap = |vm_count| NodeCapacity {
            total_cpus: 28,
            total_memory_mib: 65536,
            available_cpus: 28.0,
            available_memory_mib: 32768,
            vm_count,
            control_plane_eligible: true,
        };
        let id = Uuid::nil();
        let empty_incumbent = calculate_score(&cap(0), Duration::from_secs(3600 * 24), true, id);
        let holder_just_restarted = calculate_score(&cap(1), Duration::from_secs(0), false, id);
        assert!(holder_just_restarted > empty_incumbent, "{holder_just_restarted} <= {empty_incumbent}");
        // and a bigger store beats a smaller one, capped so it stays bounded
        assert!(calculate_score(&cap(5), Duration::ZERO, false, id) > calculate_score(&cap(1), Duration::ZERO, false, id));
        assert_eq!(state_component(10), state_component(50));
    }

    #[test]
    fn test_capacity_scoring() {
        let cap = NodeCapacity {
            total_cpus: 64,
            total_memory_mib: 256 * 1024,
            available_cpus: 32.0,
            available_memory_mib: 128 * 1024,
            vm_count: 10,
            control_plane_eligible: true,
        };

        let score = capacity_component(&cap);
        assert!(score > 0.0 && score <= 1000.0);
    }

    #[test]
    fn test_load_scoring() {
        let cap_idle = NodeCapacity {
            total_cpus: 64,
            total_memory_mib: 256 * 1024,
            available_cpus: 64.0,
            available_memory_mib: 256 * 1024,
            vm_count: 0,
            control_plane_eligible: true,
        };

        let cap_busy = NodeCapacity {
            total_cpus: 64,
            total_memory_mib: 256 * 1024,
            available_cpus: 0.0,
            available_memory_mib: 0,
            vm_count: 100,
            control_plane_eligible: true,
        };

        assert!(load_component(&cap_idle) > load_component(&cap_busy));
        assert!((load_component(&cap_idle) - 1000.0).abs() < 1.0);
        assert!((load_component(&cap_busy) - 0.0).abs() < 1.0);
    }
}
