# Discovery, election and the leader

Several xdebbied boxes on one LAN find each other over multicast, elect one
leader, and that leader makes every decision: it runs the scheduler and the
reconciler, and it owns the answer to every `/v1` cluster request. The
others are followers. A client can talk to any box; followers forward to
the leader. `xdctl` goes one step further and moves itself to the leader
before doing anything.

A box with discovery disabled (`enable_discovery = false`), or that cannot
open the multicast socket, is its own leader. Nothing else changes.

## What the leader does

| Concern | Leader | Follower |
|---|---|---|
| Reconciler (`inventory → schedule → step VMs`) | runs every `reconcile_interval_secs` | idle (`follower: reconciler idle, the leader decides` in the log) |
| `/v1/hosts`, `/v1/vms`, `/v1/volumes`, `/v1/failure-domains`, `/v1/schedule` | answers from its store | forwards to the leader verbatim, relays status, headers and body |
| `/v1/leader`, `/v1/network`, `/v1/token`, `/v1/mesh*`, `/healthz` | local | local (they describe the box you asked) |
| Token rotation (`/v1/token/rotate`) | mints, distributes to peers, retires the old one | forwards |
| DHCP responder | answers | answers unless `[network] leader_only = true` |

Forwarding adds one header, `X-Xdebbie-Forwarded-By: <hostname>`. A request
that already carries it is never forwarded again: if leadership moved
between the two hops the second box answers `503` and the client retries.
While an election is running a follower answers `503` with
`Retry-After: 2`. If the leader cannot be reached the follower answers
`502` naming it. Bearer auth is checked on the box you asked, then the
`Authorization` header travels with the request, so every box in a cluster
must share one token — `xdctl token rotate` changes it everywhere
(docs/operations.md).

## `GET /v1/leader`

```json
{"this":   {"node_id": "bef8ad05-…", "hostname": "node0.example.net", "api_url": "http://10.23.0.1:7443"},
 "state":  "follower",
 "is_leader": false,
 "leader": {"node_id": "609980bd-…", "hostname": "node1.example.net", "api_url": "http://10.23.0.2:7443"},
 "peers":  2}
```

`state` is `standalone`, `follower`, `election` or `leader`; `leader` is
`null` during an election. `api_url` is what the box advertises (below).

## `xdctl`

`xdctl` takes `XDEBBIE_URL` as a list of seeds (comma-separated), asks the
first one that answers `/v1/leader` who leads, and talks to that URL for the
rest of the command. An election in progress is waited out for up to 20 s
(`waiting for a leader election at …` on stderr). A daemon that predates
`/v1/leader` is used as-is.

`xdctl mesh …` is the exception: `/v1/mesh` is the state of one box, so it
stays on the first seed that answers and never follows the leader. To
enrol a peer on box B, point `XDEBBIE_URL` (or `--url`) at B.

```
$ xdctl leader
asked   node0.example.net (http://10.23.0.1:7443)  [follower, 2 peers]
leader  node1.example.net (http://10.23.0.2:7443)
```

## Advertised URL

Peers and clients reach a box at the URL it announces: `advertise_url` from
`xdebbied.toml` if set (use the FQDN: `https://node0.example.net:7443`),
otherwise the scheme plus the listen address, with an unspecified IP
(`0.0.0.0`) replaced by the default-route interface address.

## Protocol

UDP multicast on `239.0.0.42:7442` (`multicast_addr`), TTL 1, JSON messages
with a `type` tag. The socket is bound with `SO_REUSEADDR`/`SO_REUSEPORT`
so a replacement daemon can join while the old one is still running.

| Message | When | Carries |
|---|---|---|
| `ANNOUNCE` | every 5 s | `node_id`, `hostname`, `api_url`, `addrs`, capacity, score |
| `HEARTBEAT` | every 1 s | `node_id`, score, `is_leader` |
| `ELECTION_PROPOSE` | a follower has not heard a leader for 5 s | `node_id`, score |
| `ELECTION_VOTE` | reply to a proposal with a higher score than our own | `voted_for` |

`node_id` is a fresh UUID per process. Peers unseen for 15 s are dropped.

`api_url` is built from the sender's hostname, which may resolve nowhere
but on the sender. `addrs` lists the sender's addresses (the one its
multicast route leaves by first, then every global IPv4 address); the
receiver also notes the datagram's source address. When the URL's host does
not resolve locally the receiver swaps it for the first usable address —
source first, then `addrs` — and logs a warning naming the host. API
certificates carry every local address as a SAN, so TLS still verifies.
`xdctl leader` then shows the address form. `bootstrap`'s discovery listen
does the same, so the offered join URL is dialable. Daemons older than 0.15
send no `addrs`; their source address still works.

## Election

Bully, by score. A follower that misses leader heartbeats for 5 s proposes
itself; boxes with a lower score vote for it; a majority of the peers it
knows about makes it leader, and so does a 10 s election timeout with no
better proposal. A leader that hears a heartbeat from a higher-scored
leader steps down; a follower that hears a lower-scored leader starts an
election. A lone box therefore leads about 15 s after start.

Score = state (0 or 2000–4000) + uptime (0–1000) + capacity (0–1000) +
free resources (0–1000) + incumbent bonus (500) + UUID tiebreak (0–1).
State is 2000 for any VM in the box's own store plus 200 per VM up to ten;
it outweighs everything else combined, so while stores are per box (below)
the box that holds the cluster's VMs leads, even straight after a restart,
and an empty box only ever leads an empty cluster. Among boxes with state,
or without, longer-running, larger, less loaded ones win and the incumbent
keeps its job unless clearly beaten.

## Network requirements

- One L2 segment (TTL 1), with multicast allowed. `xdebbied bootstrap`
  adds the `ufw` rule for the discovery port from local networks; if you
  change `multicast_addr` afterwards, re-run it or add the rule yourself,
  or the box never sees a peer and leads alone.
- `7443/tcp` reachable between boxes and from clients at the advertised
  URL.

## Known gap: the store is per box

Each box keeps its own `state.json`. Everything goes through the leader, so
the leader's store is the truth and the followers' copies stop moving while
they follow. On failover the new leader starts from its own, older store.
Replicating the leader's state to followers is the next step; until then
treat a leader change as a reason to re-check `xdctl host ls` and
`xdctl vm ls`.
