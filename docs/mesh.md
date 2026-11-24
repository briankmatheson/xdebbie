# Geneve overlay mesh (over WireGuard)

How xdebbied joins every box's VM bridge into one encrypted Layer 2 network.

`xdebbied` can stitch the overlay bridge of every host ("box") into one L2
segment using Geneve tunnels, so VMs on different boxes talk as if they
shared a switch. Since 0.5.0 it can also build the **WireGuard underlay**
those tunnels ride on, so inter-box VM traffic is always encrypted. The mesh
is **on by default** (since 0.12.0): a daemon with no `[mesh]` section
meshes on the first host's bridge with a managed WireGuard underlay and zero
peers — which only pins the bridge MTU, installs a small nftables table and
creates the WireGuard link, so the box is ready to be enrolled from another
one. `enabled = false` turns it off.

## What it builds

| Object | Per | Detail |
|---|---|---|
| `gnv-bP` | peer `P` | Geneve netdev, same VNI/dstport for every peer, `remote` = peer's underlay address, `ifalias xdebbie-mesh/v1`, enslaved to the overlay bridge |
| bridge port flags | peer | `isolated on` (peers never bridge to each other → no loops), `learning/flood off` in `derived` mode (on in `learn`), `mcast_flood/bcast_flood on` |
| static FDB | peer, `derived` only | 17 entries: `52:54:00:23:PP:00` to `:0f` (reserved slot MACs) and `:fe` (the peer's bridge) |
| MTU | mesh | underlay MTU − 50 (`auto`) or a fixed value ≥ 1280; applied to tunnels, the bridge, and new VM NICs (`<mtu size=…/>`) |
| `nft table inet xdebbie_mesh` | mesh | drops UDP `dstport` arriving on any interface other than `underlay` |
| `/var/lib/xdebbie/mesh.json` | mesh | persisted spec when set through the API |

Ownership is strict: a link is ours only if its name matches `^gnv-b\d{1,3}$`
**and** its alias is `xdebbie-mesh/v1`. Anything else — including a stray
`gnv-b9` with no alias, or someone else's nft tables — is never touched; it
is reported as a `NameConflict` condition instead.

Reconciliation is derived from kernel state every 5 s (or on demand). A
daemon restart on a converged host performs **zero** kernel writes.

## WireGuard underlay (0.5.0)

Geneve itself is plaintext. Set `wireguard` and the daemon owns the underlay:

| Object | Detail |
|---|---|
| link `underlay` (default `wg-flock`) | kind wireguard, `ifalias xdebbie-mesh/v1`, MTU `wireguard.mtu` (1420) |
| key | `/var/lib/xdebbie/wg.key`, generated with `wg genkey` once, mode 0600; survives teardown so a box keeps its identity |
| address | `wireguard.address` or `10.22.BOX.1`, prefix `wireguard.prefix` (16) → the connected route sends `10.22.0.0/16` into the tunnel |
| peers | one WireGuard peer per mesh peer with a `pubkey`: `endpoint` if given, `allowed-ips REMOTE/32`, `persistent-keepalive wireguard.keepalive` (25) |

With WireGuard managed, `peers[].remote` is derived (`10.22.BOX.1`), the
underlay route check holds by construction, and `allow_plain_underlay` is
moot. Enrolment is two commands:

```bash
# on box A (box 0), then the same on box B (box 1)
xdctl mesh wg                           # prints this box's public key
# on box B: add box A
xdctl mesh peer add 0 --pubkey A_PUBKEY --endpoint box0.example.org:51820
# on box A: add box B
xdctl mesh peer add 1 --pubkey B_PUBKEY --endpoint box1.example.org:51820
```

`xdctl mesh` talks to the box named in `--url`/`XDEBBIE_URL`, not the
leader — the mesh is per-box state, so each command above must be run with
the URL of the box whose peer list it edits (on that box, the sourced
`/etc/xdebbie/xdebbie_env` already does this).

Those `xdctl` calls go to the API over HTTPS. Every box's cert is signed
by the one cluster CA, which the second box got when it was enrolled
(`xdebbied bootstrap --join https://box0.example.org:23443`, then
`xdctl approve`; see [Join a second box](getting-started/join.md)). A box that was never enrolled
cannot be reached by the others' daemons or `xdctl`.

A peer behind NAT can omit `endpoint`; it initiates towards the others and
keepalives hold the mapping. The firewall rule still restricts Geneve to the
WireGuard interface; the WireGuard port itself must be reachable on the
carrier (it is UDP `listen_port`, accepted by default policy).

`status.wireguard` shows the local pubkey/port/address and, per peer,
`endpoint`, `handshake_age_s` (`null` = never), `rx_bytes`, `tx_bytes`.
Conditions `WireguardModuleMissing` and `WireguardConflict` (the underlay
name exists but isn't a WireGuard link we own) are blocking.

## Clients (xdj)

A laptop or server that runs **xdj** is a mesh *client*, not a box of the
mesh: one WireGuard link to this box (the hub), no Geneve tunnel, and it
routes `10.23.0.0/16` through the hub. Register it in `spec.clients`, not
`spec.peers`. Step by step: [Connect a laptop with xdj](how-to/mesh-clients.md).

```console
client$ xdj key                                             # prints KEY
hub$    xdctl mesh client add --pubkey KEY --name laptop --hub-endpoint HUB_HOST
added xdj client box 3 (10.22.3.1)

run on the client:
  xdj join 3 HUB_PUBKEY HUB_HOST:51820
client$ xdj join 3 HUB_PUBKEY HUB_HOST:51820                # the line printed above, verbatim
```

`client add` picks the **lowest free box number** (1-254) that is not this
box, a peer or another client; `--box B` asks for one and is refused if it
is taken (nothing is ever replaced). Adding a key that is already a client
just reports its box. The printed line carries this box's WireGuard public
key (as `xdctl mesh wg` shows it) and `HOST:listen_port`; without
`--hub-endpoint HOST[:PORT]` it prints the placeholder `HUB_HOST:PORT` for
you to fill in (xdj ignores a literal `HUB_HOST...` and keeps the endpoint
it has).

A client is a WireGuard peer and nothing more:

| | peer | client |
|---|---|---|
| WireGuard peer, `allowed-ips 10.22.BOX.1/32`, keepalive | yes | yes |
| Geneve tunnel `gnv-bBOX`, FDB entries, probe | yes | no |
| row in `status.peers`, counts for Ready/Degraded | yes | no: a client being away is normal |
| row in `status.wireguard.peers` | yes | yes, with `"client": true` and its `name` |
| `endpoint` | usually | optional; clients initiate |

```json
"spec":   {"clients": [{"box": 3, "pubkey": "KEY", "name": "laptop"}]}
"status": {"wireguard": {"peers": [
  {"box": 3, "pubkey": "KEY", "endpoint": "198.51.100.7:40312",
   "handshake_age_s": 12, "rx_bytes": 1234, "tx_bytes": 5678,
   "client": true, "name": "laptop"}]}}
```

Rules (checked on every PUT): clients need a managed WireGuard underlay;
box 1-254 and unique across this box, the peers and the clients; `pubkey`
required, a 44-character WireGuard key, unique across peers and clients;
`name` optional, `[A-Za-z0-9._-]`, not all digits, unique. `xdctl mesh peer
add` refuses a box that belongs to a client.

`xdctl mesh ls` lists clients in their own table (box, name, key prefix,
endpoint, last handshake); `xdctl mesh client rm BOX|NAME|PUBKEY` removes
one (its WireGuard peer goes, nothing else is touched). The hub only
routes for clients with `mesh_routed` on (`/v1/settings`).

**Moving an xdj box that was added as a peer** (it shows Down/Degraded
forever, since it never answers Geneve probes), keeping its number so the
client needs no change:

```bash
xdctl mesh peer rm BOX
xdctl mesh client add --box BOX --pubkey KEY
```

### What clients are told: `[clients]`

A hub tells its xdj clients about the site: which resolver answers which
DNS zone, the search domains to try short names in (in order of
preference), and a welcome banner xdj prints once connected. xdj fetches it
after each handshake from `GET /v1/client-config/xdj` and keeps a copy.

```toml
[clients]
search = ["mesh.example.org", "int.example.org", "example.org"]
welcome = """
Welcome to the example mesh.
  dashboard:  https://dash.mesh.example.org/
"""

[[clients.dns]]
zone = "mesh.example.org"
server = "10.23.0.11"

[[clients.dns]]
zone = "int.example.org"
server = "10.23.0.243"
```

Or while running (kept in `client-config.json` beside `state_file`, which
overrides `[clients]` from then on):

```sh
xdctl mesh client-config                      # show it
xdctl mesh client-config set --zone mesh.example.org=10.23.0.11 \
    --zone int.example.org=10.23.0.243 \
    --search mesh.example.org,int.example.org,example.org \
    --welcome-file welcome.txt
```

Both `GET /v1/client-config` (JSON) and `GET /v1/client-config/xdj` (text)
are public, like `/v1/version`: a client asks before it holds a
certificate. Anyone who can reach the API can read them, so **nothing
secret belongs here**. `PUT /v1/client-config` (the whole document) needs
the cluster token or a cluster certificate. Each hub answers for itself;
it is not forwarded to the leader. Checked: zones and search domains are DNS
names, each zone once, servers are IP addresses, the banner is printable
text (tabs and newlines allowed) of at most 4 KiB.

## Configuration

Two sources, API wins:

1. `[mesh]` in `xdebbied.toml` — validated at startup; an invalid section
   stops the daemon. No section = the defaults below with `bridge` taken
   from the first `[[hosts]]` entry.
2. `PUT /v1/mesh` (what `xdctl mesh` uses) — stored in `mesh.json`, survives
   restarts, and shadows the TOML section until `DELETE /v1/mesh`.

```toml
[mesh]
enabled = true               # default
box = 0                      # 0–253, unique per host
bridge = "virbr-xdebbie"     # default: the first [[hosts]] bridge
vni = 23
dstport = 6081
underlay = "wg-flock"        # interface peers are reached through
allow_plain_underlay = false # true: you bring the underlay; WireGuard is then not managed
mtu = "auto"                 # or e.g. 1370
fdb = "derived"              # or "learn" when VM MACs are not derivable
firewall = true
probe_interval_ms = 1000
probe_timeout_ms = 800
probe_down_after = 3
probe_up_after = 3

[mesh.wireguard]             # the managed underlay; these are the defaults
listen_port = 51820
# address = "10.22.0.1"      # default 10.22.BOX.1
prefix = 16
mtu = 1420
keepalive = 25

[[mesh.peers]]
box = 1
pubkey = "EtEvrro2LlkElPgm+ge5ndqtzL2o9ehIWwyIFnKsBQU="
endpoint = "box1.example.org:51820"
# remote = "10.22.1.1"       # derived when [mesh.wireguard] is set, required otherwise
# probe = "10.23.1.1"        # default: 10.23.BOX.1 (derived) / remote (learn)

[[mesh.clients]]             # xdj; needs the managed WireGuard underlay
box = 3
pubkey = "4ZJxNbbSoUq4tgYKAa0Ow6EYwlANqldjfPGYdPZ9P2I="
name = "laptop"              # optional
# endpoint = "HOST:51820"    # optional: clients initiate
```

Validation errors are returned field-by-field, e.g.
`{"errors":[{"field":"peers[0].box","message":"must differ from local box"}]}`.

## CLI

Full list with defaults: [xdctl reference](reference/xdctl.md#mesh).

```bash
xdctl mesh ls                         # peers table + conditions
xdctl mesh describe                   # full {spec,status} JSON
xdctl mesh apply -f mesh.json         # PUT (bare spec or {spec,status} doc)
xdctl mesh peer add 2 10.22.2.1       # read-modify-write with If-Match (plain underlay)
xdctl mesh peer add 2 --pubkey K --endpoint h2:51820   # managed WireGuard: remote derived
xdctl mesh peer rm 2
xdctl mesh client add --pubkey KEY [--name NAME] [--box BOX] [--hub-endpoint HOST[:PORT]]
xdctl mesh client rm BOX|NAME|PUBKEY  # xdj clients (see "Clients (xdj)")
xdctl mesh client-config [set ...]    # what xdj clients are told (zones, search, welcome)
xdctl mesh wg                         # local WireGuard identity + per-peer handshakes
xdctl mesh reconcile                  # nudge the loop
xdctl mesh delete --yes               # tear down; falls back to [mesh] TOML
```

## API

| Method | Path | Notes |
|---|---|---|
| `GET` | `/v1/mesh` | `{spec, status}`; `status.generation` / `observed_generation` |
| `PUT` | `/v1/mesh` | body = spec; `If-Match: GENERATION` → `409` on mismatch; `400` with `errors[]` |
| `DELETE` | `/v1/mesh` | removes API state; `reenabled_from_config` tells whether TOML took over |
| `GET` | `/v1/mesh/peers/{box}` | one peer's status |
| `POST` | `/v1/mesh/reconcile` | `202`, triggers a tick |

### Status

Conditions: `Ready`, `Degraded` (some peer not Up), and blocking ones that
stop all kernel writes: `GeneveModuleMissing`, `UnderlayMissing`,
`BridgeMissing`, `WireguardModuleMissing`, `WireguardConflict`. Warnings:
`BridgeMacMismatch` (the bridge is not `52:54:00:23:BOX:fe`, so in
`derived` mode *peers* cannot reach this box's bridge address — its gateway
and DNS — fix with `virsh net-edit` on the `<mac>` element and
`ip link set BRIDGE address MAC` to apply live; bootstrap sets it when it creates the network,
from the box id the cluster CA assigned at enrolment), `NameConflict`, and `BridgeStpEnabled` — which only
stays set if turning STP off failed: the mesh **forces STP off** on its
bridge every tick, since isolated tunnel ports make it loop-free and BPDUs
crossing tunnels would only make STP misbehave. Peers carry `state` (`Pending`/`Up`/`Down`/`Error`), `rtt_ms`,
`reason` (e.g. `UnderlayRouteMismatch` — the route to `remote` leaves via a
device other than `underlay`; the tunnel is set down but never deleted),
`fdb_entries`, and `last_change`. `vms_mtu_mismatch` lists VM ports whose MTU
differs from the mesh MTU (fixed on the VM's next restart).

`status.metrics` holds a Prometheus-ready snapshot (`xdebbie_mesh_*`) until a
daemon-wide metrics endpoint exists.

## Host requirements

The `.deb` ships `/etc/modules-load.d/xdebbie-mesh.conf` (`geneve`,
`wireguard`) and depends on `iproute2`, `nftables` and `wireguard-tools`. The daemon runs as root with no
`PrivateNetwork`/`CapabilityBoundingSet` restrictions, so netlink and raw
ICMP (peer probes) work.

**`derived` FDB and VM MACs.** In `derived` mode the static FDB covers each
peer's bridge MAC and its 16 slot MACs, `52:54:00:23:BOX:00` to `:0f`, and
the tunnel ports neither learn nor flood unknown unicast. VM MACs are
derived from the VM's UUID (`52:54:00:` plus its first three bytes), not
from those slots. If VMs on different boxes cannot reach each other, set
`fdb = "learn"` on every box.

### Host firewall (ufw)

`xdebbied bootstrap` (run by postinst on install and upgrade) owns the host
firewall through ufw: default deny inbound. The LAN interface accepts only
ssh (anywhere), WireGuard (anywhere), and the API and discovery multicast
(from local networks — so a box not yet meshed still finds the cluster).
Everything else is accepted on the mesh's own interfaces only — libvirt
TLS on the underlay and the bridge, Geneve on the underlay, DHCP on the
bridge. Rules are tagged `xdebbie …`; tagged rules
bootstrap no longer wants are deleted, so a box converges on re-run. It
also removes the `XDEBBIE-INPUT` iptables chain older versions installed.
Services that run next to xdebbied (ABE, DNS, an apt proxy) add their own
rules where they are set up. VM egress (NAT, DNS forwarding) is in
[Overlay addressing and DHCP](network.md#leaving-the-box-nat-and-dns).

The symptom of a missing Geneve/WireGuard rule is a WireGuard handshake that
succeeds and `gnv-bN` tunnels whose TX counters climb while RX stays at 0
on the far side; `sudo ufw status` should show:

```text
51820/udp                  ALLOW IN    Anywhere        # xdebbie wireguard underlay
6081/udp on wg-flock       ALLOW IN    Anywhere        # xdebbie geneve on wg-flock
```

The mesh's own `inet xdebbie_mesh` nft table stays: it drops Geneve arriving
on any interface other than the underlay, independent of ufw.

### AppArmor

Debian's `wireguard-tools` confines `/usr/bin/wg` and only lets it read keys
under `/etc/wireguard`. xdebbied therefore passes the private key to
`wg set … private-key /dev/stdin` over a pipe, and the `.deb` ships
`/etc/apparmor.d/local/wg` allowing `/var/lib/xdebbie/wg.key r,` for manual
use.

### Joining bridges that already have DHCP

Once two boxes are meshed their bridges are **one L2 segment**: every DHCP
server on either bridge hears every VM's DISCOVER. Use xdebbie's own
responder (`[network]`, see [network](network.md)): it answers only for MACs in
its own VM store, so boxes never contend, and it hands out the mesh MTU as
DHCP option 26. Each box's bridge has its own /24 of the shared /16
(`10.23.0.1/16` on box 0, `10.23.1.1/16` on box 1, …; VMs at `.N.2-254`),
which is also where the derived probe (`10.23.BOX.1`) lands. Bootstrap
sets this up; a bridge at some other address makes it warn with the
`virsh net-edit` to run. If a box still runs libvirt's dnsmasq range or
`dhcpd` on the bridge, remove it before enrolling.

## Mesh-only mode

`xdebbied serve --mesh-only` runs just the mesh manager and the `/v1/mesh`
routes (no libvirt, LVM, discovery). The 3-host network-namespace harness in
`tests/mesh/` uses it:

```bash
tests/mesh/run.sh            # M1–M14 acceptance tests (re-execs under unshare -Urnm)
ONLY="M4 M8" KEEP=1 tests/mesh/run.sh
```

## Driving the mesh from another controller

A controller that manages peers itself calls its **local** daemon with
`PUT /v1/mesh` + `If-Match`, sending the full peer list. With the managed underlay each peer is just
`{box, pubkey, endpoint}` (remote derived, no `probe`), and enrol/revoke is a
single PUT — the daemon orders the WireGuard and Geneve changes itself.
Wait for `observed_generation == generation` before treating a change as
applied. Carry `spec.clients` (xdj) over from the `GET` unchanged, or the
PUT removes every client. See the module docs in
`crates/xdebbied/src/mesh/mod.rs`.
