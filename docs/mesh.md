# Geneve overlay mesh

`xdebbied` 0.4.0 can stitch the overlay bridge of every host ("box") into one
L2 segment using Geneve tunnels, so VMs on different boxes talk as if they
shared a switch. The mesh is **off by default**; enabling it with zero peers
only pins the bridge MTU and installs a small nftables table.

Design reference: *Aviary in a Box Design Specification* §6.8 and the
*XDebbie Geneve Mesh — Implementation Spec* v1.0.

## What it builds

| Object | Per | Detail |
|---|---|---|
| `gnv-b<P>` | peer `P` | Geneve netdev, same VNI/dstport for every peer, `remote` = peer's underlay address, `ifalias xdebbie-mesh/v1`, enslaved to the overlay bridge |
| bridge port flags | peer | `isolated on` (peers never bridge to each other → no loops), `learning/flood off` in `derived` mode (on in `learn`), `mcast_flood/bcast_flood on` |
| static FDB | peer, `derived` only | 17 entries: `52:54:00:23:<PP>:00..0f` (VM slots) and `:fe` (the peer's bridge) |
| MTU | mesh | underlay MTU − 50 (`auto`) or a fixed value ≥ 1280; applied to tunnels, the bridge, and new VM NICs (`<mtu size=…/>`) |
| `nft table inet xdebbie_mesh` | mesh | drops UDP `dstport` arriving on any interface other than `underlay` |
| `/var/lib/xdebbie/mesh.json` | mesh | persisted spec when set through the API |

Ownership is strict: a link is ours only if its name matches `^gnv-b\d{1,3}$`
**and** its alias is `xdebbie-mesh/v1`. Anything else — including a stray
`gnv-b9` with no alias, or someone else's nft tables — is never touched; it
is reported as a `NameConflict` condition instead.

Reconciliation is derived from kernel state every 5 s (or on demand). A
daemon restart on a converged host performs **zero** kernel writes.

## Configuration

Two sources, API wins:

1. `[mesh]` in `xdebbied.toml` — validated at startup; an invalid section
   stops the daemon.
2. `PUT /v1/mesh` (what `xdctl mesh` uses) — stored in `mesh.json`, survives
   restarts, and shadows the TOML section until `DELETE /v1/mesh`.

```toml
[mesh]
enabled = true
box = 0                      # 0–253, unique per host
bridge = "virbr-int"
vni = 23
dstport = 6081
underlay = "wg-flock"        # interface peers are reached through
allow_plain_underlay = false # true: tolerate a non-WireGuard underlay
mtu = "auto"                 # or e.g. 1370
fdb = "derived"              # or "learn" when VM MACs are not derivable
firewall = true
probe_interval_ms = 1000
probe_timeout_ms = 800
probe_down_after = 3
probe_up_after = 3

[[mesh.peers]]
box = 1
remote = "10.22.1.1"
# probe = "10.23.1.1"        # default: 10.23.<box>.1 (derived) / remote (learn)
```

Validation errors are returned field-by-field, e.g.
`{"errors":[{"field":"peers[0].box","message":"must differ from local box"}]}`.

## CLI

```
xdctl mesh ls                         # peers table + conditions
xdctl mesh get                        # full {spec,status} JSON
xdctl mesh apply -f mesh.json         # PUT (bare spec or {spec,status} doc)
xdctl mesh peer add 2 10.22.2.1       # read-modify-write with If-Match
xdctl mesh peer rm 2
xdctl mesh reconcile                  # nudge the loop
xdctl mesh delete --yes               # tear down; falls back to [mesh] TOML
```

## API

| Method | Path | Notes |
|---|---|---|
| `GET` | `/v1/mesh` | `{spec, status}`; `status.generation` / `observed_generation` |
| `PUT` | `/v1/mesh` | body = spec; `If-Match: <generation>` → `409` on mismatch; `400` with `errors[]` |
| `DELETE` | `/v1/mesh` | removes API state; `reenabled_from_config` tells whether TOML took over |
| `GET` | `/v1/mesh/peers/{box}` | one peer's status |
| `POST` | `/v1/mesh/reconcile` | `202`, triggers a tick |

### Status

Conditions: `Ready`, `Degraded` (some peer not Up), and blocking ones that
stop all kernel writes: `GeneveModuleMissing`, `UnderlayMissing`,
`BridgeMissing`. Warnings: `BridgeStpEnabled`, `BridgeMacMismatch`,
`NameConflict`. Peers carry `state` (`Pending`/`Up`/`Down`/`Error`), `rtt_ms`,
`reason` (e.g. `UnderlayRouteMismatch` — the route to `remote` leaves via a
device other than `underlay`; the tunnel is set down but never deleted),
`fdb_entries`, and `last_change`. `vms_mtu_mismatch` lists VM ports whose MTU
differs from the mesh MTU (fixed on the VM's next restart).

`status.metrics` holds a Prometheus-ready snapshot (`xdebbie_mesh_*`) until a
daemon-wide metrics endpoint exists.

## Host requirements

The `.deb` ships `/etc/modules-load.d/xdebbie-mesh.conf` (`geneve`) and
depends on `iproute2` and `nftables`. The daemon runs as root with no
`PrivateNetwork`/`CapabilityBoundingSet` restrictions, so netlink and raw
ICMP (peer probes) work. The dev host should use `bridge = "virbr-int"` and
`fdb = "learn"` until VM MACs are derivable.

## Mesh-only mode

`xdebbied serve --mesh-only` runs just the mesh manager and the `/v1/mesh`
routes (no libvirt, LVM, discovery). The 3-host network-namespace harness in
`tests/mesh/` uses it:

```
tests/mesh/run.sh            # M1–M13 acceptance tests (re-execs under unshare -Urnm)
ONLY="M4 M8" KEEP=1 tests/mesh/run.sh
```

## Flock contract

`flockd` calls its **local** daemon with `PUT /v1/mesh` + `If-Match`, sending
the full peer list (`remote = 10.22.<box>.1`, no `probe`). Enroll: WireGuard
peer up → PUT. Revoke: PUT without the peer → wait for
`observed_generation == generation` → remove the WireGuard peer. See the
module docs in `crates/xdebbied/src/mesh/mod.rs`.
