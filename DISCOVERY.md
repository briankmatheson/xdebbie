# XDebbie Auto-Discovery and Leader Election

XDebbie v0.3.0+ implements automatic peer discovery using multicast UDP and leader election using the Bully algorithm.

## Quick Start

Auto-discovery is **enabled by default**. All xdebbied nodes on the same network will automatically discover each other and elect a leader.

```bash
# Install on multiple hosts
sudo dpkg -i xdebbied_0.3.0-1_amd64.deb

# Nodes will automatically:
# 1. Announce themselves on multicast 239.0.0.42:7442
# 2. Discover other nodes
# 3. Elect a leader based on score
# 4. Maintain HA with automatic failover
```

## Configuration

Discovery settings in `/etc/xdebbie/xdebbied.toml`:

```toml
# Enable/disable discovery (default: true)
enable_discovery = true

# Multicast address for discovery (default: 239.0.0.42:7442)
multicast_addr = "239.0.0.42:7442"
```

### Disable Discovery

To run in standalone mode (single node):

```toml
enable_discovery = false
```

## How It Works

### Discovery Protocol

XDebbie uses multicast UDP on **239.0.0.42:7442** for peer discovery:

1. **ANNOUNCE** (every 5s)
   - Node announces itself with hostname, API URL, capacity
   - Sent when node starts and periodically

2. **HEARTBEAT** (every 1s)
   - Indicates node is alive
   - Includes current score and leader status
   - Leader sends heartbeats to maintain leadership

3. **ELECTION_PROPOSE** (during election)
   - Node proposes itself as leader
   - Includes current score

4. **ELECTION_VOTE** (during election)
   - Vote for a leader candidate
   - Majority wins

### Message Format

All messages are JSON over UDP:

```json
{
  "type": "ANNOUNCE",
  "node_id": "550e8400-e29b-41d4-a716-446655440000",
  "hostname": "node1",
  "api_url": "http://192.168.1.10:7443",
  "capacity": {
    "total_cpus": 64,
    "total_memory_mib": 262144,
    "available_cpus": 32.0,
    "available_memory_mib": 131072,
    "vm_count": 10,
    "control_plane_eligible": true
  },
  "score": 2457.382901,
  "sequence": 42
}
```

### Leader Election (Bully Algorithm)

XDebbie uses the **Bully algorithm** with score-based ranking:

#### Score Calculation

Each node's score is calculated from multiple components:

1. **Uptime (0-1000 points)**
   - First hour: 0-250 points (linear)
   - 1-24 hours: 250-750 points (linear)
   - 24+ hours: 750-1000 points (logarithmic)
   - Preference for stable, long-running nodes

2. **Capacity (0-1000 points)**
   - CPU capacity: 0-500 points (normalized to 128 cores)
   - Memory capacity: 0-500 points (normalized to 512 GiB)
   - Preference for larger nodes

3. **Load (0-1000 points)**
   - Inverse of utilization
   - Idle nodes score higher
   - Preference for less loaded nodes

4. **Incumbent Bonus (+500 points)**
   - Current leader gets preference
   - Reduces leadership churn
   - Only applied if node is currently leader

5. **UUID Tiebreaker (0-1 points)**
   - Deterministic ordering when scores equal
   - Uses first 8 bytes of UUID

**Total Score Range**: 0-3501 points

#### Election Process

1. **Normal Operation**
   - Leader sends heartbeats every 1s
   - Followers listen for leader heartbeats
   - No election needed

2. **Leader Failure Detection**
   - If no leader heartbeat for 5s, node starts election
   - Node transitions to ELECTION state
   - Sends ELECTION_PROPOSE with its score

3. **Election Voting**
   - Nodes vote for the highest-scored candidate
   - If candidate has lower score, node starts own election
   - Votes are sent as ELECTION_VOTE messages

4. **Election Victory**
   - Node becomes leader when it receives majority votes
   - Immediately starts sending heartbeats as leader
   - Followers recognize new leader from heartbeats

5. **Election Timeout**
   - If no winner after 10s, highest-scored node becomes leader
   - Prevents deadlock situations

6. **Automatic Failover**
   - When leader fails, new election starts within 5s
   - New leader elected within ~10s total
   - Less than 15s total failover time

### State Diagram

```
          ┌─────────┐
          │INACTIVE │
          └────┬────┘
               │ activate()
               v
          ┌────────┐      leader_heartbeat_timeout()      ┌────────────┐
          │FOLLOWER│──────────────────────────────────────>│  ELECTION  │
          └────┬───┘                                       └─────┬──────┘
               ^                                                 │
               │          higher_scored_leader                   │ has_majority()
               │          heartbeat_received                     │ or timeout
               │                                                 v
               │                                            ┌────────┐
               └────────────────────────────────────────────│ LEADER │
                              step_down()                   └────────┘
```

## Network Requirements

### Multicast

- **Address**: 239.0.0.42:7442
- **Protocol**: UDP
- **TTL**: 1 (local network only)
- **Packet Size**: Maximum 64 KB (typical: <1 KB)

### Firewall Rules

Allow multicast UDP on port 7442:

```bash
# iptables
sudo iptables -A INPUT -p udp -d 239.0.0.42 --dport 7442 -j ACCEPT

# firewalld
sudo firewall-cmd --permanent --add-port=7442/udp
sudo firewall-cmd --reload

# ufw
sudo ufw allow 7442/udp
```

### Network Switches

Ensure multicast is enabled on network switches:
- **IGMP snooping**: May need configuration
- **Multicast flooding**: Should be enabled for discovery subnet

## Monitoring

### Check Discovery Status

```bash
# View discovery logs
sudo journalctl -u xdebbied -f | grep -i discovery

# Example output:
# Nov 08 16:00:00 node1 xdebbied[1234]: Discovery engine initialized: node_id=550e8400... multicast=239.0.0.42:7442
# Nov 08 16:00:00 node1 xdebbied[1234]: Starting discovery engine
# Nov 08 16:00:05 node1 xdebbied[1234]: Discovered new peer: node2 (650e8400...)
# Nov 08 16:00:10 node1 xdebbied[1234]: Won election with majority vote, becoming leader
```

### Check Leader Status

```bash
# View election logs
sudo journalctl -u xdebbied -f | grep -i election

# Example output:
# Nov 08 16:00:00 node1 xdebbied[1234]: Election state transition: INACTIVE -> FOLLOWER
# Nov 08 16:00:10 node1 xdebbied[1234]: Leader heartbeat timeout, starting election
# Nov 08 16:00:10 node1 xdebbied[1234]: Election state transition: FOLLOWER -> ELECTION
# Nov 08 16:00:11 node1 xdebbied[1234]: Won election with majority vote, becoming leader
# Nov 08 16:00:11 node1 xdebbied[1234]: Election state transition: ELECTION -> LEADER
```

### Score Calculation Details

```bash
# Check score components in logs
sudo journalctl -u xdebbied -f | grep "Score calculation"

# Example output:
# Score calculation: uptime=875.3 capacity=750.0 load=850.0 leader_bonus=500.0 uuid=0.382901 total=2975.7
```

## Troubleshooting

### Nodes Not Discovering Each Other

1. **Check multicast is enabled**:
   ```bash
   ip maddr show
   # Should show 239.0.0.42 on relevant interface
   ```

2. **Check firewall**:
   ```bash
   sudo iptables -L -n | grep 7442
   ```

3. **Test multicast reachability**:
   ```bash
   # On receiver
   socat UDP4-RECVFROM:7442,ip-add-membership=239.0.0.42:0.0.0.0,fork -
   
   # On sender
   echo "test" | socat - UDP4-DATAGRAM:239.0.0.42:7442
   ```

4. **Check network interface**:
   ```bash
   # Discovery binds to 0.0.0.0:7442 by default
   # Ensure multicast routing is correct
   route -n | grep 224.0.0.0
   ```

### Multiple Leaders

If you see multiple leaders (split-brain):

1. **Check network partitions**:
   ```bash
   # Ensure all nodes can reach each other
   ping <other-node>
   ```

2. **Check system clocks**:
   ```bash
   # Ensure NTP is synchronized
   timedatectl status
   ```

3. **Restart discovery**:
   ```bash
   sudo systemctl restart xdebbied
   ```

### Election Thrashing

If leader changes too frequently:

1. **Check node stability**: Ensure nodes aren't crashing
2. **Check network quality**: High packet loss can cause issues
3. **Check resource pressure**: Overloaded nodes may become unresponsive

## Architecture Notes

### Why Multicast?

- **Zero configuration**: No need to configure peer lists
- **Dynamic membership**: Nodes join/leave automatically
- **Low overhead**: Single packet reaches all nodes
- **LAN-only**: TTL=1 keeps traffic local

### Why Bully Algorithm?

- **Simple**: Easy to implement and understand
- **Score-based**: Can incorporate multiple factors
- **No coordinator**: Fully distributed
- **Self-stabilizing**: Converges to optimal leader

### Leader Responsibilities

The leader node:
- Runs the active scheduler (assigns VMs to hosts)
- Serves as the primary API endpoint (backups proxy to leader)
- Maintains authoritative state
- Coordinates cluster-wide operations

### Backup Nodes

Non-leader nodes:
- Maintain shadow state (read-only)
- Proxy API requests to leader
- Ready to take over if leader fails
- Continue local VM reconciliation

## API Integration

Future versions will expose discovery/election status via API:

```bash
# Get cluster status (planned)
xdctl cluster status

# Get peer list (planned)
xdctl cluster peers

# Get current leader (planned)
xdctl cluster leader
```

## Migration from Pre-0.3.0

XDebbie 0.3.0 is backward compatible with earlier versions. Discovery can be disabled:

```toml
# /etc/xdebbie/xdebbied.toml
enable_discovery = false
```

This allows gradual migration:
1. Upgrade nodes to 0.3.0 with discovery disabled
2. Enable discovery on one node at a time
3. Verify cluster formation and leader election
4. Complete migration

## Security Considerations

### Current Implementation (v0.3.0)

- **No authentication**: Discovery messages are unauthenticated
- **LAN-only**: TTL=1 prevents routing beyond local network
- **Best for**: Trusted networks (private data centers, VPCs)

### Future Enhancements

- Message signing (HMAC)
- Shared secret for cluster membership
- TLS for inter-node communication
- Rate limiting and DoS protection

### Network Isolation

For security, run XDebbie discovery on isolated management network:
- Separate VLAN for control plane
- Firewall rules to restrict access
- No direct internet access required

## Performance

### Network Overhead

Per node:
- **Announce**: ~500 bytes every 5s = ~100 bytes/s
- **Heartbeat**: ~200 bytes every 1s = ~200 bytes/s
- **Total**: ~300 bytes/s per node (multicast)

For 100-node cluster:
- **Total bandwidth**: ~30 KB/s (negligible)
- **Packets per second**: ~200 (well within limits)

### CPU Overhead

- Message processing: <0.1% CPU per node
- Score calculation: <0.01% CPU per calculation
- Election overhead: Occasional spikes during elections

### Convergence Time

- **Peer discovery**: <5s (next announce interval)
- **Leader election**: 5-15s (heartbeat timeout + election)
- **Total failover**: <15s worst case

## References

- [Bully Algorithm](https://en.wikipedia.org/wiki/Bully_algorithm)
- [IP Multicast](https://en.wikipedia.org/wiki/IP_multicast)
- [IGMP](https://en.wikipedia.org/wiki/Internet_Group_Management_Protocol)
