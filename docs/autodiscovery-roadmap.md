# Autodiscovery Implementation Roadmap

## Quick Summary

Transform xdebbied from single-node to **self-organizing cluster** with:
- Multicast discovery (239.0.0.42:7442)
- Leader election (Bully algorithm)
- Hot standby backups
- Automatic failover

## Implementation Phases

### Phase 1: Multicast Discovery ⏱️ 2-3 days

**Goal**: Nodes discover each other and share capacity

**Files to Create**:
```
src/xdebbie/crates/xdebbied/src/discovery/
├── mod.rs              - Public interface, coordinator
├── multicast.rs        - UDP multicast socket handling
├── messages.rs         - Protocol message serde types
├── peer.rs             - Peer state tracking
└── announcer.rs        - Periodic announcement sender
```

**Dependencies to Add** (Cargo.toml):
```toml
socket2 = "0.5"          # For multicast socket options
```

**Key Tasks**:
- [x] Design protocol (DONE - see XDEBBIED_AUTODISCOVERY_DESIGN.md)
- [ ] Implement multicast socket (IPv4, join group 239.0.0.42)
- [ ] Create message types (ANNOUNCE, HEARTBEAT, etc.)
- [ ] Send announcements every 5s with capacity
- [ ] Receive and parse announcements from peers
- [ ] Track active peers (timeout after 15s)
- [ ] Expose peer list via API (GET /cluster/peers)

**Testing**:
```bash
# Terminal 1: Start node1
xdebbied serve --bootstrap

# Terminal 2: Start node2
xdebbied serve --bootstrap

# Terminal 3: Check they found each other
curl http://localhost:7443/cluster/peers
# Should show both nodes
```

**Acceptance Criteria**:
- ✓ Two nodes discover each other within 10 seconds
- ✓ Peer list shows both nodes with capacity
- ✓ Peers removed after 15s of silence
- ✓ Works on multicast-enabled networks

---

### Phase 2: Leader Election ⏱️ 3-4 days

**Goal**: One node becomes leader, others become backups

**Files to Create**:
```
src/xdebbie/crates/xdebbied/src/election/
├── mod.rs              - Election coordinator
├── state.rs            - State machine (OBSERVING → ELECTING → LEADER/BACKUP)
├── scoring.rs          - Election score calculation
├── bully.rs            - Bully algorithm implementation
└── lease.rs            - Leader lease management
```

**Key Tasks**:
- [ ] Implement election score calculation (uptime + capacity + load)
- [ ] State machine transitions (STARTUP → OBSERVING → ELECTING → LEADER/BACKUP)
- [ ] Bully algorithm: detect leader failure, start election, send proposals, vote
- [ ] Leader heartbeat sender (every 1s)
- [ ] Leader heartbeat monitor (timeout after 5s)
- [ ] Lease expiration logic
- [ ] Tie-breaking via UUID comparison

**Testing**:
```bash
# Start 3 nodes
for i in 1 2 3; do
  xdebbied serve --bootstrap --config /tmp/node$i.toml &
done

# Wait for election
sleep 10

# Check cluster status
curl http://localhost:7443/cluster/status
# Should show 1 LEADER, 2 BACKUP

# Kill leader
pkill -f node1

# Wait for re-election
sleep 10

# Check new leader elected
curl http://localhost:7443/cluster/status
```

**Acceptance Criteria**:
- ✓ Election completes in <10 seconds
- ✓ Only one leader elected
- ✓ Highest score node becomes leader
- ✓ Failover to backup within 15 seconds
- ✓ No split-brain (multiple leaders)

---

### Phase 3: Scheduler Coordination ⏱️ 2-3 days

**Goal**: Only leader runs active scheduler, backups are passive

**Files to Update**:
```
src/xdebbie/crates/xdebbied/src/
├── reconcile.rs        - Check role before reconciling
├── api.rs              - Proxy scheduling requests to leader
└── main.rs             - Start discovery/election tasks
```

**Key Tasks**:
- [ ] Modify reconciler: only run if role == LEADER
- [ ] API proxy: forward POST /vms to leader if backup
- [ ] Leader broadcasts VM placements via multicast
- [ ] Backups update shadow state from broadcasts
- [ ] State convergence logic

**Testing**:
```bash
# Start cluster
# Create VM via backup node
curl -X POST http://backup:7443/vms -d '{"name": "test", ...}'

# Should be proxied to leader
# VM should appear on all nodes
for node in node1 node2 node3; do
  curl http://$node:7443/vms
done
```

**Acceptance Criteria**:
- ✓ Only leader runs reconciler
- ✓ Backup nodes proxy to leader
- ✓ VMs visible on all nodes
- ✓ State converges within 10 seconds

---

### Phase 4: Fault Tolerance & Polish ⏱️ 2-3 days

**Goal**: Handle edge cases, make production-ready

**Key Tasks**:
- [ ] Split-brain detection and recovery
- [ ] Network partition handling
- [ ] Graceful leader stepdown
- [ ] Cluster health monitoring
- [ ] Metrics and logging
- [ ] Documentation

**Testing**:
- Chaos testing (kill random nodes)
- Network partitions (iptables)
- Clock skew
- Message loss

**Acceptance Criteria**:
- ✓ Handles network partitions gracefully
- ✓ Detects and recovers from split-brain
- ✓ Comprehensive logging
- ✓ Metrics exposed

---

## Development Workflow

### Step 1: Create Discovery Module (Day 1-2)

```bash
# Create directory structure
mkdir -p src/xdebbie/crates/xdebbied/src/discovery

# Start with message types
# Then multicast socket
# Then announcer
# Then peer tracking
# Finally coordinator
```

### Step 2: Test Discovery in Isolation (Day 2-3)

```bash
# Build and test
cargo build --package xdebbied
cargo test --package xdebbied discovery

# Manual test with 2 nodes
./test-discovery.sh
```

### Step 3: Add Election (Day 3-5)

```bash
mkdir -p src/xdebbie/crates/xdebbied/src/election

# Implement state machine
# Implement bully algorithm
# Integrate with discovery
```

### Step 4: Integration (Day 6-8)

```bash
# Wire into main.rs
# Update reconciler
# Update API handlers
# End-to-end testing
```

### Step 5: Documentation & Cleanup (Day 9-10)

```bash
# Write user docs
# Add examples
# Performance tuning
# Release
```

---

## Configuration Changes

### Before (Single Node)
```toml
listen = "0.0.0.0:7443"
state_file = "/var/lib/xdebbie/state.json"

[[hosts]]
name = "localhost"
uri = "qemu:///system"
```

### After (Cluster Mode)
```toml
listen = "0.0.0.0:7443"
state_file = "/var/lib/xdebbie/state.json"

# Auto-discovery via multicast
[discovery]
enabled = true
multicast_addr = "239.0.0.42:7442"
multicast_ttl = 1
announce_interval_secs = 5

# Leader election
[election]
enabled = true
heartbeat_interval_secs = 1
election_timeout_secs = 5

# Local host (auto-discovered by others)
[[hosts]]
name = "localhost"
uri = "qemu:///system"
```

**Bootstrap will generate this automatically!**

---

## API Changes

### New Endpoints

```bash
# Get cluster status
GET /cluster/status
{
  "state": "stable",
  "leader": {"id": "...", "name": "node1"},
  "nodes": [
    {"id": "...", "role": "leader", "capacity": {...}},
    {"id": "...", "role": "backup", "capacity": {...}}
  ]
}

# Get peer list
GET /cluster/peers
[
  {"id": "...", "name": "node1", "addr": "http://10.0.0.5:7443", "role": "leader"},
  {"id": "...", "name": "node2", "addr": "http://10.0.0.6:7443", "role": "backup"}
]

# Trigger election (admin)
POST /cluster/election/trigger
```

### Modified Behavior

```bash
# POST /vms on backup node
# → Proxies to leader
# → Returns result

# GET /hosts
# → Shows all discovered hosts, not just local
```

---

## Migration Strategy

### Option 1: Opt-In (Safer)

Discovery disabled by default:
```toml
[discovery]
enabled = false  # Default
```

Users opt-in:
```bash
# Edit config
[discovery]
enabled = true

# Restart
systemctl restart xdebbied
```

### Option 2: Auto-Enable (Better UX)

Bootstrap enables by default:
```bash
xdebbied bootstrap
# Automatically creates config with discovery enabled
```

**Recommendation**: Auto-enable, but provide `--no-discovery` flag for single-node use.

---

## Success Criteria

### Phase 1 Complete When:
- [ ] Two nodes discover each other via multicast
- [ ] Peer list accessible via API
- [ ] Capacity information shared
- [ ] Peers timeout correctly

### Phase 2 Complete When:
- [ ] One leader elected from 3 nodes
- [ ] Leader sends heartbeats
- [ ] Backups detect leader failure
- [ ] Re-election works within 15s

### Phase 3 Complete When:
- [ ] Only leader runs scheduler
- [ ] Backup proxies VM creation to leader
- [ ] State converges across cluster

### Phase 4 Complete When:
- [ ] Split-brain prevented
- [ ] Network partitions handled
- [ ] Production-ready
- [ ] Documented

---

## Risk Mitigation

### Risk: Multicast not working on all networks

**Mitigation**: 
- Detect multicast availability at startup
- Fall back to unicast discovery (manual peer list)
- Document network requirements

### Risk: Split-brain during partition

**Mitigation**:
- Implement quorum checks (require majority)
- Detect multiple leaders via announcements
- Force re-election if detected
- Add fencing (block split-brain writes)

### Risk: Performance degradation

**Mitigation**:
- Benchmark multicast overhead (<1% CPU)
- Rate limit announcements
- Use efficient serialization (bincode or msgpack)
- Profile regularly

---

## Timeline

**Total**: ~10-12 days

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| 1. Discovery | 2-3 days | Multicast working, peer list |
| 2. Election | 3-4 days | Leader elected, failover works |
| 3. Coordination | 2-3 days | Scheduler on leader only |
| 4. Polish | 2-3 days | Production ready |

---

## Next Immediate Steps

1. **Create discovery module structure**
   ```bash
   mkdir -p src/xdebbie/crates/xdebbied/src/discovery
   ```

2. **Add dependencies to Cargo.toml**
   ```toml
   socket2 = "0.5"
   ```

3. **Implement message types** (messages.rs)
   - ANNOUNCE message struct
   - Serde serialization

4. **Implement multicast socket** (multicast.rs)
   - Join group 239.0.0.42:7442
   - Send/receive datagrams

5. **Create announcer** (announcer.rs)
   - Periodic task sending ANNOUNCE
   - Include capacity from libvirt

Ready to start coding? Let me know and I'll begin with Phase 1!
