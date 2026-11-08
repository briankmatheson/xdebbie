# qfodufk8sd Autodiscovery and Leader Election Design

## Overview

Enable qfodufk8sd instances to automatically discover each other via multicast, share capacity information, and elect a leader for scheduling coordination.

## Goals

1. **Automatic Discovery**: Instances find each other without manual configuration
2. **Capacity Sharing**: Each node announces its resources (CPUs, memory, disk)
3. **Leader Election**: One node becomes the scheduler, others are hot backups
4. **Fault Tolerance**: Automatic failover if leader dies
5. **Zero Config**: Works out of the box with multicast-enabled networks

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Multicast Discovery                          │
│                    239.0.0.42:7442                              │
└─────────────────────────────────────────────────────────────────┘
       ▲                    ▲                    ▲
       │                    │                    │
       │ Announce          │ Announce          │ Announce
       │ Heartbeat         │ Heartbeat         │ Heartbeat
       │                    │                    │
┌──────┴─────┐      ┌──────┴─────┐      ┌──────┴─────┐
│  node1     │      │  node2     │      │  node3     │
│  ────────  │      │  ────────  │      │  ────────  │
│  LEADER    │◄────►│  BACKUP    │◄────►│  BACKUP    │
│  Score: 100│      │  Score: 90 │      │  Score: 80 │
│            │      │            │      │            │
│  API :7443 │      │  API :7443 │      │  API :7443 │
│  Scheduler │      │  Standby   │      │  Standby   │
│  Active    │      │  Watching  │      │  Watching  │
└────────────┘      └────────────┘      └────────────┘
      │                    │                    │
      │                    │                    │
      ▼                    ▼                    ▼
   libvirt              libvirt              libvirt
  qemu:///system       qemu:///system       qemu:///system
```

## Multicast Protocol

### Multicast Group
- **Address**: `239.0.0.42` (IPv4 multicast)
- **Port**: `7442`
- **TTL**: `1` (local subnet only, or configurable)

### Message Types

#### 1. ANNOUNCE (sent every 5 seconds)
```json
{
  "type": "announce",
  "version": 1,
  "node": {
    "id": "uuid-v4",
    "name": "hostname",
    "listen_addr": "http://10.0.0.5:7443",
    "started_at": "2026-11-08T19:50:00Z",
    "sequence": 123
  },
  "capacity": {
    "cpus": 32,
    "memory_mib": 65536,
    "disk_gib": 500,
    "reserved_cpus": 2,
    "reserved_memory_mib": 4096,
    "available_cpus": 30,
    "available_memory_mib": 61440
  },
  "allocation": {
    "used_cpus": 8,
    "used_memory_mib": 16384,
    "vm_count": 4
  },
  "role": "leader|backup|standby",
  "leader_id": "uuid-of-current-leader",
  "election_score": 100,
  "cluster_state": "stable|electing|recovering"
}
```

#### 2. HEARTBEAT (sent every 1 second by leader)
```json
{
  "type": "heartbeat",
  "version": 1,
  "leader_id": "uuid-v4",
  "sequence": 456,
  "timestamp": "2026-11-08T19:50:01Z",
  "lease_expires_at": "2026-11-08T19:50:06Z",
  "cluster_state": {
    "total_nodes": 3,
    "total_vms": 12,
    "healthy_nodes": 3
  }
}
```

#### 3. ELECTION_PROPOSE (during leader election)
```json
{
  "type": "election_propose",
  "version": 1,
  "candidate_id": "uuid-v4",
  "candidate_name": "node2",
  "election_score": 95,
  "timestamp": "2026-11-08T19:50:02Z"
}
```

#### 4. ELECTION_VOTE (during leader election)
```json
{
  "type": "election_vote",
  "version": 1,
  "voter_id": "uuid-v4",
  "candidate_id": "uuid-v4",
  "timestamp": "2026-11-08T19:50:03Z"
}
```

## Leader Election Algorithm

### Election Score Calculation

```rust
fn calculate_election_score(&self) -> u64 {
    // Higher score = more likely to become leader
    let mut score = 0u64;
    
    // 1. Uptime (max 40 points)
    // Older nodes preferred (stability)
    let uptime_secs = (now() - self.started_at).as_secs();
    score += (uptime_secs / 60).min(40); // 1 point per minute, max 40
    
    // 2. Capacity (max 30 points)
    // More resources = better leader
    score += (self.capacity.cpus as u64).min(20);
    score += (self.capacity.memory_mib / 4096).min(10);
    
    // 3. Load (max 20 points)
    // Less loaded = better leader
    let cpu_free_pct = 100 - (self.allocation.used_cpus * 100 / self.capacity.cpus);
    score += (cpu_free_pct as u64 * 20 / 100).min(20);
    
    // 4. Previously elected (max 10 points)
    // Prefer stable leadership
    if self.was_leader {
        score += 10;
    }
    
    // 5. Tie-breaker: UUID comparison
    // Deterministic for same scores
    score += (self.node_id.as_bytes()[0] as u64) % 10;
    
    score
}
```

### Election Process

1. **Leader Failure Detected** (no heartbeat for 5 seconds)
   - All nodes enter `ELECTING` state
   - Each calculates its election score
   - Highest score becomes candidate

2. **Propose Phase** (2 seconds)
   - Candidate sends `ELECTION_PROPOSE` with score
   - Other nodes compare with their own score
   - If someone has higher score, they propose too

3. **Vote Phase** (2 seconds)
   - Nodes vote for candidate with highest score
   - Send `ELECTION_VOTE` messages
   - Track received votes

4. **Decision Phase** (1 second)
   - Candidate with majority votes becomes leader
   - Leader sends first `HEARTBEAT`
   - Others become backups
   - Transition to `STABLE` state

5. **Tie Breaking**
   - If scores are equal, use UUID comparison (deterministic)
   - Node with lexicographically smallest UUID wins

### Leader Responsibilities

1. **Schedule VM Placements** (active scheduler)
2. **Send Heartbeats** (every 1 second)
3. **Monitor Cluster Health**
4. **Accept API Requests** (all nodes accept, but leader makes decisions)
5. **Coordinate State** (eventually consistent)

### Backup Responsibilities

1. **Monitor Leader Heartbeats** (detect failures)
2. **Maintain Shadow State** (ready to take over)
3. **Accept API Requests** (proxy to leader for scheduling)
4. **Stand Ready for Election**

## State Machine

### Node States

```
┌─────────────┐
│   STARTUP   │  Initial state
└──────┬──────┘
       │
       │ Start multicast listener
       │
       ▼
┌─────────────┐
│  OBSERVING  │  Listen for announcements (10s)
└──────┬──────┘
       │
       │ No leader detected OR leader timeout
       │
       ▼
┌─────────────┐
│  ELECTING   │  Participate in leader election (5s)
└──────┬──────┘
       │
       ├─► Won election ──────┐
       │                      │
       └─► Lost election      │
                              ▼
       ┌──────────────┐  ┌─────────────┐
       │    BACKUP    │  │   LEADER    │
       │   (Standby)  │  │  (Active)   │
       └──────┬───────┘  └──────┬──────┘
              │                 │
              │                 │ Send heartbeats
              │ Monitor leader  │ Run scheduler
              │                 │
              │                 ▼
              │          Leader healthy
              │                 │
              │                 │ Lost quorum / Fatal error
              ▼                 ▼
       Leader failed      ┌─────────────┐
              │           │  RECOVERING │
              │           └──────┬──────┘
              │                  │
              └──────────────────┘
                       │
                       ▼
                  ELECTING
```

## Implementation Plan

### Phase 1: Multicast Discovery (Week 1)

**Files to Create:**
```
src/qfodufk8s/crates/qfodufk8sd/src/discovery/
├── mod.rs              - Main discovery module
├── multicast.rs        - Multicast send/receive
├── messages.rs         - Protocol message types
├── peer.rs             - Peer tracking
└── config.rs           - Discovery configuration
```

**Features:**
- Multicast socket setup (IPv4 239.0.0.42:7442)
- Send ANNOUNCE messages (every 5s)
- Receive and parse announcements
- Maintain peer list (active peers)
- Detect peer timeouts (15s = 3 missed announcements)

### Phase 2: Leader Election (Week 2)

**Files to Create:**
```
src/qfodufk8s/crates/qfodufk8sd/src/election/
├── mod.rs              - Election coordinator
├── raft_lite.rs        - Simplified Raft algorithm
├── scoring.rs          - Election score calculation
├── voting.rs           - Vote tracking
└── state.rs            - Node state machine
```

**Features:**
- Election score calculation
- Leader failure detection (heartbeat timeout)
- Election protocol (propose, vote, decide)
- State transitions (OBSERVING → ELECTING → LEADER/BACKUP)
- Tie-breaking via deterministic UUID comparison

### Phase 3: Scheduler Coordination (Week 3)

**Files to Update:**
```
src/qfodufk8s/crates/qfodufk8sd/src/
├── reconcile.rs        - Only run on LEADER
├── api.rs              - Proxy scheduling to leader
└── store.rs            - Shared state awareness
```

**Features:**
- Leader runs active reconciler
- Backups run passive reconciler (shadow state)
- API requests on backup → proxy to leader
- Leader broadcasts VM placement decisions
- Eventually consistent state across cluster

### Phase 4: Fault Tolerance (Week 4)

**Features:**
- Split-brain detection (multiple leaders)
- Network partition handling
- Graceful leader handoff (voluntary step-down)
- VM state reconciliation after leader change
- Cluster health monitoring

## Configuration

### New Config Fields

```toml
[discovery]
enabled = true
multicast_addr = "239.0.0.42:7442"
multicast_ttl = 1
announce_interval_secs = 5
heartbeat_interval_secs = 1
peer_timeout_secs = 15
election_timeout_secs = 5

[election]
enabled = true
algorithm = "raft_lite"  # or "bully"
min_quorum = 2           # Minimum nodes for election
lease_duration_secs = 5  # Leader lease time
```

### Bootstrap Updates

Add discovery config generation:

```rust
// In config_gen.rs
pub fn generate_config_with_discovery(...) -> String {
    format!(
        r#"...
[discovery]
enabled = true
multicast_addr = "239.0.0.42:7442"
announce_interval_secs = 5
peer_timeout_secs = 15

[election]
enabled = true
min_quorum = 1  # Allow single-node clusters
"#
    )
}
```

## API Changes

### New Endpoints

```rust
// GET /cluster/status
{
  "nodes": [
    {
      "id": "uuid",
      "name": "node1",
      "role": "leader",
      "capacity": {...},
      "allocation": {...},
      "last_seen": "2026-11-08T19:50:00Z"
    }
  ],
  "leader": {
    "id": "uuid",
    "name": "node1",
    "listen_addr": "http://10.0.0.5:7443"
  },
  "state": "stable"
}

// GET /cluster/peers
[
  {"id": "uuid", "name": "node1", "addr": "http://10.0.0.5:7443", "role": "leader"},
  {"id": "uuid", "name": "node2", "addr": "http://10.0.0.6:7443", "role": "backup"}
]

// POST /cluster/election/trigger (admin only)
{
  "reason": "manual trigger"
}
```

### Modified Endpoints

```rust
// POST /vms (on backup node)
// → HTTP 307 Temporary Redirect → leader
// → Or: proxy request internally and return result

// GET /vms (on any node)
// → Return local view (eventually consistent)
```

## Security Considerations

### Multicast Security

1. **Network Isolation**
   - Use private multicast range (239.0.0.0/8)
   - TTL=1 for subnet-only by default
   - Optionally: shared secret in announcements

2. **Message Authentication** (future)
   - HMAC signatures on all messages
   - Shared cluster secret (from config or bootstrap)
   - Reject unsigned/invalid messages

3. **Rate Limiting**
   - Max 10 announcements/sec from any peer
   - Drop duplicate messages (by sequence)
   - Prevent denial of service

### Leader Security

1. **Lease-Based Leadership**
   - Leader must renew lease every N seconds
   - Lease expires if heartbeats stop
   - Prevents zombie leaders

2. **Split-Brain Detection**
   - Track announced leader ID
   - If multiple leaders detected → force re-election
   - Log warning and alert

3. **API Authentication**
   - Same token auth on all nodes
   - Leader election doesn't bypass auth
   - Inter-node communication uses shared secret

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_election_score_calculation() {
        // Test score algorithm
    }
    
    #[test]
    fn test_leader_timeout_detection() {
        // Test heartbeat monitoring
    }
    
    #[test]
    fn test_tie_breaking() {
        // Test deterministic UUID comparison
    }
}
```

### Integration Tests

```bash
# Three-node cluster
./test_cluster.sh 3

# Start three instances
qfodufk8sd serve --bootstrap &  # node1
qfodufk8sd serve --bootstrap &  # node2
qfodufk8sd serve --bootstrap &  # node3

# Wait for election
sleep 10

# Check cluster status
curl http://localhost:7443/cluster/status

# Kill leader
pkill -f "qfodufk8sd.*node1"

# Wait for re-election
sleep 10

# Verify new leader
curl http://localhost:7443/cluster/status
```

### Chaos Testing

- Kill random nodes
- Network partitions (iptables)
- Clock skew simulation
- Message loss/delay (tc netem)
- Split-brain scenarios

## Performance Considerations

### Network Overhead

- **Announcements**: 5s interval × ~500 bytes = ~100 bytes/sec per node
- **Heartbeats**: 1s interval × ~200 bytes = ~200 bytes/sec (leader only)
- **Elections**: Rare, ~5s duration, ~2KB total traffic

**Total**: ~300 bytes/sec per node (negligible)

### CPU Overhead

- Multicast listener: <1% CPU
- Election algorithm: <1% CPU during elections (rare)
- Peer tracking: O(n) nodes, trivial for <100 nodes

### Memory Overhead

- Peer list: ~1KB per peer × 100 peers = 100KB
- Message buffers: ~10KB
- State machine: ~1KB

**Total**: ~200KB additional memory

## Migration Path

### Backward Compatibility

1. **Discovery disabled by default** (opt-in)
   ```toml
   [discovery]
   enabled = false  # Default
   ```

2. **Fall back to single-node mode**
   - If discovery disabled, works like before
   - Manual host registration still works

3. **Gradual rollout**
   - Enable discovery on one node → see peers
   - Enable on second node → election starts
   - Add more nodes as needed

### Upgrade Procedure

```bash
# 1. Upgrade binary on all nodes
scp qfodufk8sd node{1,2,3}:/usr/local/bin/

# 2. Update config on all nodes
cat >> /etc/qfodufk8s/qfodufk8sd.toml <<EOF
[discovery]
enabled = true
EOF

# 3. Restart nodes one at a time
systemctl restart qfodufk8sd  # node1
sleep 30
systemctl restart qfodufk8sd  # node2
sleep 30
systemctl restart qfodufk8sd  # node3

# 4. Verify cluster formed
curl http://node1:7443/cluster/status
```

## Future Enhancements

1. **IPv6 Multicast** (ff02::42)
2. **mDNS/DNS-SD Integration** (for non-multicast networks)
3. **Gossip Protocol** (for larger clusters, >100 nodes)
4. **Raft Consensus** (full implementation for stronger guarantees)
5. **Quorum-Based Decisions** (require majority for VM placement)
6. **Multi-Region Support** (unicast between regions, multicast within)

## Decision Points

### Algorithm Choice: Raft-Lite vs Bully

**Raft-Lite** (Recommended):
- ✅ Well-understood
- ✅ Log-based, ordered decisions
- ✅ Strong consistency guarantees
- ❌ More complex implementation

**Bully Algorithm** (Simpler):
- ✅ Simple to implement
- ✅ Fast elections
- ✅ Good for small clusters
- ❌ Less robust in partitions

**Decision**: Start with **Bully** for speed, consider Raft later for scale.

### Single vs Multi-Region

**Single Region** (MVP):
- Multicast works within subnet
- Low latency (<10ms)
- Simple implementation

**Multi-Region** (Future):
- Unicast between regions
- Higher latency (50-200ms)
- Regional leaders + global coordinator

**Decision**: Start with **single region**, design for future expansion.

## Success Metrics

- ✅ Zero-config clustering (no manual host registration)
- ✅ Leader election completes in <10 seconds
- ✅ Failover completes in <15 seconds
- ✅ No split-brain scenarios in testing
- ✅ Cluster stable with 3-10 nodes
- ✅ <1% CPU overhead
- ✅ <500KB memory overhead per node

## Summary

This design enables qfodufk8sd to form **self-organizing, fault-tolerant clusters** with:

1. **Zero configuration** - Nodes discover each other via multicast
2. **Automatic leadership** - Best node elected as scheduler
3. **High availability** - Hot backups ready for instant failover
4. **Scalability** - Support 3-100 nodes efficiently
5. **Simplicity** - Bully algorithm, lightweight protocol

Next step: Implement Phase 1 (Multicast Discovery) in `src/qfodufk8s/crates/qfodufk8sd/src/discovery/`.
