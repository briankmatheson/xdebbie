# The leader

One box decides: it schedules, reconciles, and answers every cluster
request. The others forward to it, and take over if it goes.

With discovery off (the default) every box is its own leader, state
`standalone`, and decides only about the requests sent to it. With
discovery on, boxes on one LAN elect one leader
([Discovery](../network/discovery.md)).

## What you need

- For an elected leader: discovery on, on every box, on one L2 segment.

## Find the leader

```bash
xdctl leader
```

```text
asked   box0.example.org (https://box0.example.org:23443)  [follower, 2 peers]
leader  box1.example.org (https://box1.example.org:23443)
```

`GET /v1/leader` gives the same as JSON: `this`, `state` (`standalone`,
`follower`, `election` or `leader`), `is_leader`, `leader` (`null` during
an election) and `peers`.

## What the leader does

| Concern | Leader | Follower |
|---|---|---|
| The reconciler | runs every `reconcile_interval_secs` | idle |
| `/v1/hosts`, `/v1/vms`, `/v1/volumes`, `/v1/failure-domains`, `/v1/schedule`, `/v1/qos`, `/v1/token/rotate` | answers | forwards to the leader and relays the reply |
| `/v1/enroll...` | forwarded to the CA box (`ca_url`) | the same |
| `/v1/leader`, `/v1/network`, `/v1/token`, `/v1/enroll/token`, `/v1/mesh...`, `/v1/settings`, `/v1/version`, `/v1/client-config...`, `/v1/ca`, `/v1/cert`, `/v1/agent/...`, `/healthz` | answers for itself | answers for itself |
| The DHCP responder | answers | answers, unless `[network] leader_only = true` |

Forwarding adds the header `X-Xdebbie-Forwarded-By: HOSTNAME`. A request
that already carries it is never forwarded again: if leadership moved
between the two hops, the second box answers 503 and the client retries.
During an election a follower answers 503 with `Retry-After: 2`; if it
cannot reach the leader, 502 naming it. A relayed body may be up to 16 MiB.
The bearer token is checked on the box you asked and travels on with the
request, so every box must share one token.

`xdctl` takes `XDEBBIE_URL` as a list of seeds, asks the first that answers
who leads, and talks to the leader for the rest of the command. It waits up
to 20 seconds for an election. `xdctl mesh`, `xdctl version` and
`xdctl ca --chain` stay on the box you named; `xdctl image` talks to no
daemon at all.

## Election

Bully, by score. A follower that misses the leader's heartbeat for 5
seconds proposes itself. Boxes with a lower score vote for it; a box with a
higher score proposes itself in turn (unless it heard a leader in the last
5 seconds), and a candidate that hears a better proposal stands down and
votes for it. A majority of the boxes a candidate knows of (`n / 2 + 1`)
makes it leader, unless a box it heard in the last 5 seconds outranks it;
so does 10 seconds without a better proposal. A leader that hears a
higher-scored leader steps down.

A lone box leads 15 to 17 seconds after it starts, and so does the
survivor of a two-box cluster. With three or more boxes, the next best
takes over about 6 seconds after the last heartbeat.

The score:

| Part | Points |
|---|---|
| State: VMs in the box's own store | 0 with none; 2000 plus 200 per VM, up to ten VMs |
| Uptime | 0-1000: 250 in the first hour, 750 at a day, then slowly more |
| Capacity | 0-1000: CPUs (128 is full marks) and memory (512 GiB) |
| Free resources | 0-1000 |
| Already the leader | 500 |
| Tiebreak | 0-1, from the node id |

State outweighs everything else, so the box that holds the cluster's VMs
leads, even straight after a restart, and an empty box only ever leads an
empty cluster.

## Followers keep up

Each box keeps its own `state.json`, and only the leader's moves. Two
tasks on every follower keep a leader change from handing the cluster to a
box that knows nothing:

- **The registrar.** Every 5 seconds a follower `PUT`s its own `[[hosts]]`
  entries to the leader, as other boxes must see them: `uri` is
  `qemu+tls://BRIDGE_ADDRESS/system` and `agent` is its own API URL. It
  sends them again every 60 seconds in case the leader's store went
  backwards. (`bootstrap --join` does it once, with the CA box.)
- **The mirror.** Every 5 seconds a follower copies `/v1/hosts`, `/v1/vms`
  and `/v1/volumes` from the leader into its own store, keeping its own
  `[[hosts]]` entries as configured locally. A new leader starts from a
  copy at most 5 seconds old, and every box's DHCP responder has the
  records it answers from.

The leader drives a host that has an `agent` through that box's daemon
(`POST /v1/agent/OP`): thin-pool disks, NVMe connects, seeds and base
images all live where the VM runs.

## What it does not do yet

- There is no replicated store: a new leader starts from a copy up to 5
  seconds old.
- Discovery needs one L2 segment with multicast. Boxes on different
  networks cannot elect a leader together.
- With discovery off, each box decides alone about what is sent to it.
  Point every client at the same box.

## If it goes wrong

- If `xdctl` prints *waiting for a leader election*, then an election is
  running; it waits up to 20 seconds.
- If a request answers 502 naming the leader, then the box you asked
  cannot reach it.
- If two boxes both say `leader`, then they do not hear each other
  ([Discovery](../network/discovery.md#if-it-goes-wrong)).

## Reference

[`GET /v1/leader`](../reference/api.md#leader),
[`enable_discovery`](../reference/configuration.md#top-level-keys),
[`xdctl leader`](../reference/xdctl.md#boxes-and-the-ca).
