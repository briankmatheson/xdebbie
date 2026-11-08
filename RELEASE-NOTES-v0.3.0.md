# XDebbie v0.3.0 Release Notes

**Release Date**: 2026-11-08

## What's New

### 🎯 Auto-Discovery & Leader Election

XDebbie v0.3.0 introduces **zero-configuration clustering** with automatic peer discovery and leader election.

#### Key Features

- **Multicast Discovery**: Nodes automatically find each other on 239.0.0.42:7442
- **Leader Election**: Bully algorithm with score-based ranking (uptime + capacity + load)
- **High Availability**: Automatic failover in <15 seconds
- **Zero Configuration**: No peer lists, no coordination service, no manual setup

#### How It Works

1. **Install the package** on multiple Debian 13 nodes
2. **Nodes auto-discover** each other via multicast
3. **Highest-scored node becomes leader** using Bully algorithm
4. **Leader manages scheduling** while backups maintain shadow state
5. **Automatic failover** if leader fails

```bash
# Install on 3 nodes
sudo dpkg -i xdebbied_0.3.0-1_amd64.deb

# Nodes automatically:
# - Discover each other
# - Elect a leader
# - Maintain HA with failover
```

### Score-Based Leader Election

Leaders are elected based on a composite score (0-3501 points):

| Component | Points | Description |
|-----------|--------|-------------|
| **Uptime** | 0-1000 | Longer-running nodes preferred |
| **Capacity** | 0-1000 | Larger nodes (CPU + memory) preferred |
| **Load** | 0-1000 | Less-loaded nodes preferred |
| **Incumbent Bonus** | +500 | Current leader gets preference (reduces churn) |
| **UUID Tiebreaker** | 0-1 | Deterministic ordering when equal |

Example:
- Node with 7 days uptime, 64 cores, 50% load: ~2800 points
- Node with 1 hour uptime, 16 cores, 90% load: ~600 points
- Leader (incumbent): adds +500 bonus

### Network Protocol

XDebbie uses UDP multicast for discovery:

```
239.0.0.42:7442 (multicast UDP)
├── ANNOUNCE (every 5s)
│   └── Hostname, API URL, capacity, score
├── HEARTBEAT (every 1s)
│   └── Score, leader status
├── ELECTION_PROPOSE (during election)
│   └── Candidate score
└── ELECTION_VOTE (during election)
    └── Vote for candidate
```

All messages are JSON over UDP, human-readable for debugging.

## Installation

### New Installations

```bash
# Download package
wget https://github.com/yourusername/xdebbie/releases/download/v0.3.0/xdebbied_0.3.0-1_amd64.deb

# Install on each node
sudo dpkg -i xdebbied_0.3.0-1_amd64.deb

# Check discovery
sudo journalctl -u xdebbied -f | grep discovery
```

### Upgrading from v0.2.x

Discovery is **enabled by default** but backward compatible:

```bash
# Upgrade
sudo dpkg -i xdebbied_0.3.0-1_amd64.deb

# Service auto-restarts with discovery enabled
```

To disable discovery (standalone mode):

```toml
# /etc/xdebbie/xdebbied.toml
enable_discovery = false
```

### Upgrading from v0.1.x (qfodufk8s)

1. **Backup state**: `sudo cp /var/lib/qfodufk8s/state.json ~/state-backup.json`
2. **Remove old package**: `sudo apt-get remove qfodufk8sd`
3. **Install new package**: `sudo dpkg -i xdebbied_0.3.0-1_amd64.deb`
4. **Migrate config** (see [docs/migration.md](docs/migration.md))

## Configuration

### Discovery Settings

```toml
# /etc/xdebbie/xdebbied.toml

# Enable/disable discovery (default: true)
enable_discovery = true

# Multicast address (default: 239.0.0.42:7442)
multicast_addr = "239.0.0.42:7442"
```

### Network Requirements

**Firewall**:
```bash
# Allow multicast UDP
sudo iptables -A INPUT -p udp -d 239.0.0.42 --dport 7442 -j ACCEPT
```

**Switches**:
- IGMP snooping: May need configuration
- Multicast flooding: Should be enabled

**Routers**:
- TTL=1 (local network only, no routing required)

## Monitoring

### Check Cluster Status

```bash
# View discovery logs
sudo journalctl -u xdebbied -f | grep discovery

# Example output:
# Discovery engine initialized: node_id=550e8400...
# Discovered new peer: node2 (650e8400...)
# Starting discovery engine
```

### Check Leader Election

```bash
# View election logs  
sudo journalctl -u xdebbied -f | grep election

# Example output:
# Election state transition: INACTIVE -> FOLLOWER
# Leader heartbeat timeout, starting election
# Won election with majority vote, becoming leader
# Election state transition: ELECTION -> LEADER
```

### Check Node Scores

```bash
# View score calculations
sudo journalctl -u xdebbied | grep "Score calculation"

# Example output:
# Score calculation: uptime=875.3 capacity=750.0 load=850.0 leader_bonus=500.0 uuid=0.382901 total=2975.7
```

## Performance Impact

### Network Overhead

Per node:
- ANNOUNCE: ~100 bytes/s
- HEARTBEAT: ~200 bytes/s
- **Total**: ~300 bytes/s per node

For 100-node cluster:
- **Total bandwidth**: ~30 KB/s (negligible)
- **Packets/sec**: ~200 (well within limits)

### CPU Overhead

- Message processing: <0.1% CPU
- Score calculation: <0.01% CPU  
- Election: Occasional spikes during leader changes

### Convergence Time

- **Peer discovery**: <5 seconds
- **Leader election**: 5-15 seconds
- **Total failover**: <15 seconds worst case

## Breaking Changes

**None**. XDebbie v0.3.0 is fully backward compatible with v0.2.x.

Discovery can be disabled for gradual migration or standalone operation.

## Known Issues

### Multicast Not Working

**Symptom**: Nodes don't discover each other

**Solutions**:
1. Check firewall: `sudo iptables -L -n | grep 7442`
2. Test multicast: See [DISCOVERY.md](DISCOVERY.md#troubleshooting)
3. Verify IGMP on switches
4. Check network interface routing

### Multiple Leaders (Split-Brain)

**Symptom**: Multiple nodes think they're leader

**Causes**:
- Network partition between nodes
- Clock skew (NTP not synchronized)
- High packet loss

**Solutions**:
1. Check network connectivity: `ping <other-node>`
2. Sync clocks: `sudo systemctl restart systemd-timesyncd`
3. Restart services: `sudo systemctl restart xdebbied`

## Security Considerations

### Current Implementation

- **No authentication**: Discovery messages are unauthenticated
- **LAN-only**: TTL=1 prevents routing beyond local network
- **Best for**: Trusted networks (private data centers, VPCs)

### Recommendations

1. **Network isolation**: Run on dedicated management VLAN
2. **Firewall rules**: Restrict UDP 7442 to cluster nodes
3. **Physical security**: Prevent unauthorized network access

### Future Enhancements (Planned)

- Message signing (HMAC)
- Shared secret for cluster membership
- TLS for inter-node communication
- Rate limiting and DoS protection

## Documentation

- **[DISCOVERY.md](DISCOVERY.md)**: Complete discovery/election documentation
- **[BOOTSTRAP.md](BOOTSTRAP.md)**: Self-bootstrapping guide
- **[README.md](README.md)**: Updated with discovery features
- **[CHANGELOG.md](CHANGELOG.md)**: Detailed change log

## Testing

All existing tests pass, plus new tests for:
- ✅ Election state transitions
- ✅ Voting and majority calculation
- ✅ Uptime scoring
- ✅ Capacity scoring
- ✅ Load scoring

```bash
# Run tests
cargo test --package xdebbied

# Output:
# test result: ok. 13 passed; 0 failed; 0 ignored
```

## Binary Size

- **xdebbied**: 9.1 MB (was 8.9 MB in v0.2.0)
- **xdctl**: 6.6 MB (unchanged)
- **Total package**: 3.5 MB (compressed)

Size increase due to:
- Discovery engine code
- Election state machine
- Multicast socket handling
- UUID and num_cpus dependencies

## Credits

Built with:
- Rust 1.84
- Tokio for async runtime
- serde_json for message serialization
- UDP multicast for discovery
- Bully algorithm for leader election

Inspired by:
- XBill game (the X11 game from the 90s)
- Debian (our primary target platform)
- Self-forming cloud architectures

## What's Next (v0.4.0)

Planned features for next release:
- [ ] API endpoints for cluster status (`xdctl cluster status`)
- [ ] Web UI for cluster visualization
- [ ] Metrics and Prometheus integration
- [ ] Message authentication (HMAC)
- [ ] Leader proxy mode (API requests auto-forward to leader)
- [ ] Scheduled leader rotation
- [ ] Multi-region support with regional leaders

## Get Involved

- **GitHub**: https://github.com/yourusername/xdebbie
- **Issues**: https://github.com/yourusername/xdebbie/issues
- **Discussions**: https://github.com/yourusername/xdebbie/discussions

## License

GPL-3.0-or-later

## Acknowledgments

Special thanks to the early testers and contributors who helped make v0.3.0 possible!
