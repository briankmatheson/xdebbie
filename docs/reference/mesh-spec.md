# The mesh spec and status

The JSON document `GET /v1/mesh` returns and `PUT /v1/mesh` takes: every
field, its default, and what is checked.

The mesh is per box: these routes answer for the box you ask and are never
forwarded. `xdctl mesh describe` prints the whole document; `xdctl mesh
apply -f FILE` sends one (a bare spec, or a `{spec, status}` document).
How the mesh behaves is in [The mesh](../network/mesh.md).

## The document

```json
{
  "spec": {
    "enabled": true,
    "box": 0,
    "networks": [{"bridge": "virbr-xdebbie", "vni": 23}],
    "dstport": 6081,
    "underlay": "wg-flock",
    "allow_plain_underlay": false,
    "mtu": "auto",
    "fdb": "derived",
    "firewall": true,
    "probe": {"interval_ms": 1000, "timeout_ms": 800, "down_after": 3, "up_after": 3},
    "wireguard": {"listen_port": 51820, "prefix": 16, "mtu": 1420, "keepalive": 25},
    "peers": [{"box": 1, "remote": "10.22.1.1", "pubkey": "KEY", "endpoint": "box1.example.org:51820"}],
    "clients": [{"box": 3, "pubkey": "KEY", "name": "laptop"}],
    "blackholes": ["10.22.0.0/16", "10.23.0.0/16", "10.24.0.0/16"]
  },
  "status": {
    "source": "api",
    "generation": 7,
    "observed_generation": 7,
    "mtu": 1370,
    "conditions": [{"type": "Ready", "status": true, "reason": "AllPeersUp", "last_transition": "..."}],
    "peers": [{"box": 1, "remote": "10.22.1.1", "probe": "10.23.1.1", "device": "gnv-b1",
               "ifindex": 12, "state": "Up", "rtt_ms": 0.4, "last_change": "...", "fdb_entries": 17}],
    "vms_mtu_mismatch": [],
    "wireguard": {"device": "wg-flock", "pubkey": "KEY", "listen_port": 51820, "address": "10.22.0.1",
                  "peers": [{"box": 3, "pubkey": "KEY", "endpoint": "198.51.100.7:40312",
                             "handshake_age_s": 12, "rx_bytes": 1234, "tx_bytes": 5678,
                             "client": true, "name": "laptop"}]},
    "metrics": {"generation": 7, "observed_generation": 7, "mtu_bytes": 1370,
                "reconcile_total": {"ok": 120}, "reconcile_actions_total": {"fdb_add": 17},
                "peers": {"1": {"up": 1, "state": "Up", "rtt_seconds": 0.0004,
                                "probe_failures_total": 0, "tunnel_rx_bytes_total": 0,
                                "tunnel_tx_bytes_total": 0}}}
  }
}
```

The spec's keys are snake_case, like the `[mesh]` table, but nested where
the table is flat: `[mesh] bridge` and `vni` are `networks[0]`, and
`probe_*` is `probe.*`.

## Spec

| Field | Default | Checked |
|---|---|---|
| `enabled` | `true` | |
| `box` | `0` | 0-253 |
| `networks` | one, `{bridge: "br-int", vni: 23}` (from the config: the first host's bridge) | exactly one; `bridge` 1-15 characters, no spaces; `vni` 1-16777215 |
| `dstport` | `6081` | 1-65535 |
| `underlay` | `"wg-flock"` | 1-15 characters, no spaces |
| `allow_plain_underlay` | `false` | |
| `mtu` | `"auto"` | `"auto"`, or at least 1280 and at most the underlay MTU minus 50 |
| `fdb` | `"derived"` | `"derived"` or `"learn"` |
| `firewall` | `true` | |
| `probe.interval_ms` | `1000` | above 0 |
| `probe.timeout_ms` | `800` | above 0, not above `interval_ms` |
| `probe.down_after`, `probe.up_after` | `3` | at least 1 |
| `wireguard` | present unless `allow_plain_underlay` | when present the mesh manages the underlay |
| `wireguard.listen_port` | `51820` | 1-65535 |
| `wireguard.address` | `10.22.BOX.1` | |
| `wireguard.prefix` | `16` | 8-30 |
| `wireguard.mtu` | `1420` | at least 1330, so the overlay keeps 1280 |
| `wireguard.keepalive` | `25` | seconds; `0` is off |
| `peers[].box` | required | 0-253, not this box, unique |
| `peers[].remote` | `10.22.BOX.1` with `wireguard` | required without it |
| `peers[].probe` | `10.23.BOX.1` (`derived`), `remote` (`learn`) | |
| `peers[].pubkey` | | required with `wireguard`: a 32-byte base64 key (44 characters ending in `=`), unique |
| `peers[].endpoint` | none | `host:port` or `[v6]:port`, port not 0 |
| `clients[].box` | required | 1-254, unique across this box, peers and clients |
| `clients[].pubkey` | required | as for peers; unique across peers and clients |
| `clients[].name` | none | 1-63 of letters, digits, `.`, `_`, `-`; not all digits; unique |
| `clients[].endpoint` | none | as for peers |
| `blackholes` | the three mesh networks | IPv4 networks, prefix 1-32, no host bits |

## Status

| Field | Meaning |
|---|---|
| `source` | `api` (from `mesh.json`) or `config` (from `[mesh]`) |
| `generation`, `observed_generation` | the spec's version, and the version the kernel matches; a change is applied when they are equal |
| `mtu` | the overlay MTU in force |
| `conditions[]` | `type`, `status`, `reason`, `message`, `last_transition` ([Conditions](../network/mesh.md#conditions)) |
| `peers[]` | per peer: `box`, `remote`, `probe`, `device`, `ifindex`, `state` (`Pending`, `Up`, `Down`, `Error`), `reason`, `rtt_ms`, `last_change`, `fdb_entries`, `route_dev` (plain underlay: where the kernel routes `remote`) |
| `vms_mtu_mismatch[]` | VM ports whose MTU is not the mesh's |
| `wireguard` | `device`, `pubkey`, `listen_port`, `address`, and per peer `box`, `pubkey`, `endpoint`, `handshake_age_s` (absent: never), `rx_bytes`, `tx_bytes`, `client`, `name` |
| `metrics` | a Prometheus-style snapshot (`xdebbie_mesh_*`) until a metrics endpoint exists: reconcile results (`ok`, `error`, `noop`), actions by kind, per-peer up, state, RTT, probe failures and tunnel bytes |

## Calls

| Call | Answers |
|---|---|
| `GET /v1/mesh` | 200 `{spec, status}` |
| `PUT /v1/mesh` | 200 `{spec, status}`; 400 `{"errors": [{"field", "message"}]}`, for example `{"field": "peers[0].box", "message": "0 is the local box number"}`; 409 when `If-Match: GENERATION` is not the current generation; 400 when `If-Match` is not a number |
| `DELETE /v1/mesh` | 200 `{status, reenabled_from_config}`: the API spec is forgotten and `[mesh]` applies again if present |
| `GET /v1/mesh/peers/{box}` | one peer's status |
| `POST /v1/mesh/reconcile` | 202; reconcile now |

An xdj client certificate may read `GET /v1/mesh`; everything else needs
the token or a box certificate.

## Reference

[`[mesh]` keys](configuration.md#mesh), [`xdctl mesh`](xdctl.md#mesh).
