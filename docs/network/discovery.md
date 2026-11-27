# Discovery

Boxes on one LAN find each other over signed multicast, so they can elect
one leader.

Discovery is off by default (`enable_discovery = false`): each box then
leads alone. Turn it on when you want the boxes to pick one leader and
fail over. What the leader does is in [The leader](../compute/leader.md).

## What you need

- The boxes on one L2 segment, with multicast allowed (TTL 1).
- One cluster token on every box (enrolment gives them that).
- Clocks within 30 seconds of each other. Run NTP.

## Turn it on

1. On each box, switch it on at runtime:

   ```bash
   curl --cacert "$XDEBBIE_CA" -H "Authorization: Bearer $XDEBBIE_TOKEN" \
        -H 'Content-Type: application/json' \
        -X PUT -d '{"discovery": true}' "$XDEBBIE_URL/v1/settings"
   ```

   It is kept in `settings.json` and also opens the discovery port in ufw.
   `enable_discovery = true` in the config does the same at startup.

2. Check:

   ```bash
   xdctl leader
   ```

## What you should see

```text
asked   box0.example.org (https://box0.example.org:23443)  [follower, 2 peers]
leader  box1.example.org (https://box1.example.org:23443)
```

A lone box leads 15 to 17 seconds after it starts.

## The protocol

UDP multicast to `239.0.0.42:23442` (`multicast_addr`), TTL 1, JSON
messages with a `type`:

| Message | When | Carries |
|---|---|---|
| `ANNOUNCE` | every 5 s | `node_id`, `hostname`, `api_url`, `addrs`, capacity, score |
| `HEARTBEAT` | every 1 s | `node_id`, score, `is_leader` |
| `ELECTION_PROPOSE` | a follower has heard no leader for 5 s | `node_id`, score |
| `ELECTION_VOTE` | reply to a better proposal | `voted_for` |

`node_id` is a new UUID each time the daemon starts. A peer silent for 15
seconds is dropped. The socket uses `SO_REUSEADDR` and `SO_REUSEPORT`, so a
new daemon can join while the old one is still stopping. A datagram is at
most 65,536 bytes.

`api_url` is built from the sender's hostname, which may resolve only on
the sender. `addrs` lists its addresses (the one its multicast route leaves
by first, then every global IPv4 address), and the receiver notes the
datagram's source too. When the host in `api_url` does not resolve, the
receiver uses the source address, then `addrs`, and logs a warning naming
the host. API certificates carry every local address, so TLS still
verifies.

## Advertised URL

Other boxes and `xdctl` reach a box at the URL it announces:
`advertise_url` if set (bootstrap writes `https://FQDN:23443`), otherwise
the scheme plus the listen address, with `0.0.0.0` replaced by the address
of the default-route interface.

## Signing

Anyone on the LAN can send to the group, so a box with a token signs every
datagram and drops any that do not verify:

```json
{"v":1,"ts":1763400000,"msg":"{\"type\":\"HEARTBEAT\",...}","mac":"...64 hex digits..."}
```

- **Key**: `HMAC-SHA256(token, "xdebbie discovery v1")`, derived from the
  cluster token, never the token itself. `mac` is
  `HMAC-SHA256(key, "v1" || ts as 8 bytes big-endian || msg)`.
- **Rotation**: receivers try the current token's key, then the previous
  one's. During a rotation senders sign with the previous token, which
  every box still holds, and switch once it is retired everywhere.
- **Replay**: `ts` is Unix seconds; a datagram more than 30 seconds off the
  receiver's clock is dropped. Inside that window each sender's `sequence`
  must rise; a repeat is dropped. Senders are remembered 75 seconds after
  they go quiet.
- **Dropped datagrams**: the first from each source address is logged as a
  warning with the reason; the rest at debug.
- **No token**: a box without `token_file` sends and hears only plain
  messages.
- **Upgrades**: daemons from before signing send plain JSON. While a
  cluster has both, set `discovery_accept_unsigned = true` on the new ones,
  and remove it when the last box is upgraded.
- **Bootstrap** listens before it holds a token, so it reads announces
  without checking them. They only become a join URL to offer; the join
  itself is what gets checked (the CA fingerprint).

## Tests

`crates/xdebbied/src/discovery/sim.rs` runs real engines on a simulated LAN
with a paused clock; `sim_tests.rs` holds the scenarios (signing, replay,
clock skew, elections, partitions, bootstrap's listen).
`XDEBBIE_SIM_TRACE=1` prints every box's log in simulated time. Tests that
need the real network are ignored unless asked for:
`XDEBBIE_TEST_MULTICAST=1` (multicast loopback, 23442/udp open) and
`XDEBBIE_TEST_DNS=1`, with `cargo test -p xdebbied -- --ignored`.

## If it goes wrong

- If every box says it leads, then they do not hear each other: check one
  L2 segment, multicast, the ufw rule for 23442/udp, and clocks within 30
  seconds. The log names the reason for the first dropped datagram from
  each sender.
- If the log warns about unsigned datagrams after an upgrade, then a box
  still runs an old daemon; see *Upgrades* above.
- If boxes are on different networks, then they cannot elect a leader
  together. Leave discovery off and point every client at one box.

## Reference

[`enable_discovery`, `multicast_addr`, `discovery_accept_unsigned`](../reference/configuration.md#top-level-keys),
[`/v1/settings`](../reference/api.md#settings),
[The leader](../compute/leader.md).
