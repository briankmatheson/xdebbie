# Configuration file: xdebbied.toml

Every key `xdebbied` reads from `/etc/xdebbie/xdebbied.toml`, with its type,
default and meaning.

The file is TOML. `xdebbied bootstrap` writes it on the first start, with a
comment on every line it sets. Set another path with `xdebbied -c PATH` or
the `XDEBBIED_CONFIG` environment variable.

Rules that apply to the whole file:

- **Unknown keys are errors.** Every table rejects keys it does not know, so
  a typo stops the daemon. It exits with status 78 and `systemctl status
  xdebbied` names the key.
- **Defaults below are the daemon's.** Where bootstrap writes a different
  value, the "Bootstrap writes" column says so.
- **Runtime overrides.** Two files beside `state_file` take precedence over
  this one once they exist: `settings.json` (from `PUT /v1/settings`) for
  `enable_discovery` and `[network] mesh_routed`, and `mesh.json` (from
  `PUT /v1/mesh`) for the whole `[mesh]` table. `client-config.json` (from
  `PUT /v1/client-config`) overrides `[clients]`.
- **`bootstrap --force` keeps your settings.** It regenerates the file and
  carries over `volume_group`, `thin_pool`, `tls_cert`, `tls_key`,
  `advertise_url`, `ca_url`, `policy`, `reconcile_interval_secs`,
  `host_stale_secs` and the `[network]`, `[mesh]` and `[[hosts]]` tables.
  Anything else you added must be re-added. The old file is kept as
  `xdebbied.toml.bak-TIMESTAMP`.

## Top-level keys

| Key | Type | Default | Bootstrap writes | Description |
|---|---|---|---|---|
| `listen` | address:port | the default-route interface's address, port 23443 (`127.0.0.1:23443` if there is no default route) | `"0.0.0.0:23443"` | Where the API listens. `0.0.0.0` listens on every address. |
| `advertise_url` | URL | scheme plus `listen`, with `0.0.0.0` replaced by the default-route address | `"https://FQDN:23443"` | The URL other boxes and `xdctl` use to reach this box. Announced in discovery and `GET /v1/leader`. |
| `state_file` | path | `/var/lib/xdebbie/state.json` | same | Hosts, VMs and volumes. Its directory also holds `mesh.json`, `settings.json`, `enroll.json`, `wg.key` and the other runtime files. |
| `token_file` | path | unset: no authentication (lab use only) | `/etc/xdebbie/token` | The cluster's API bearer token. |
| `settings_token_file` | path | unset | commented out | A second token that opens `/v1/settings` and nothing else. |
| `enroll_token_file` | path | unset | not written | A token that lists, approves and denies **xdj client** enrolments and nothing else. |
| `schedule_token_file` | path | unset | commented out | A token that opens the placement dry run, `POST /v1/schedule`, and nothing else. |
| `tls_cert` | path | unset | `/etc/xdebbie/api-cert.pem` | PEM certificate chain. With `tls_key`, the API serves HTTPS. Without both, plain HTTP. |
| `tls_key` | path | unset | `/etc/xdebbie/api-key.pem` | PEM private key for `tls_cert`. |
| `ca_file` | path | `/etc/xdebbie/api-ca.pem` | not written | The cluster CA. Trusted when this box calls another one, and exported as `XDEBBIE_CA`. |
| `peer_ca_dir` | path | `/etc/xdebbie/peers` | not written | Extra CA certificates (`*.pem`) to trust. Rarely needed. |
| `ca_key` | path | `/etc/xdebbie/api-ca-key.pem` | not written | The cluster CA's private key. Exists only on the first box; it signs enrolments. |
| `ca_url` | URL | unset | set by `bootstrap --join` | API of the box that holds the CA key. Enrolment calls that arrive here are forwarded there. |
| `policy` | `"spread"` or `"pack"` | `"spread"` | `"spread"` | Placement policy. `spread` prefers the emptiest host; `pack` the fullest. |
| `reconcile_interval_secs` | integer | `5` | `5` | Seconds between reconcile passes on the leader. |
| `host_stale_secs` | integer | `60` | `60` | A host not reached for this long is unhealthy and gets no new VMs. |
| `virsh` | string | `"virsh"` | `"virsh"` | The `virsh` binary. |
| `nvme` | string | `"nvme"` | not written | The `nvme` binary, for ABE disks. |
| `qemu_img` | string | `"qemu-img"` | not written | The `qemu-img` binary, for ABE disks. |
| `nvme_sys` | string | `"/sys/class/nvme"` | not written | sysfs directory listing NVMe controllers. |
| `seed_command` | array of strings | `["cloud-localds", "--network-config={net}", "--vendor-data={vendor}", "{out}", "{user}", "{meta}"]` | same | Builds the NoCloud seed ISO. `{out}`, `{user}`, `{meta}`, `{net}` and `{vendor}` are replaced with file paths. |
| `enable_discovery` | boolean | `false` | `false` | Multicast discovery and leader election. See [Discovery](../network/discovery.md) and [The leader](../compute/leader.md). `PUT /v1/settings` changes it at runtime. |
| `multicast_addr` | address:port | `"239.0.0.42:23442"` | same | Discovery group and port. |
| `discovery_accept_unsigned` | boolean | `false` | not written | Also hear unsigned discovery messages. Set it only while a rolling upgrade still has daemons older than signed discovery. |
| `volume_group` | string | unset | not written | LVM volume group for VM disks. Unset: qcow2 files in the libvirt pool. Set it together with `thin_pool` ([Local disks](../storage/local.md)). |
| `thin_pool` | string | unset | not written | Thin pool inside `volume_group`. |

`policy = "binpack"` is not valid; the value is `"pack"`. (Releases up to
0.21.0 write a comment that says `"binpack"`.)

## `[network]`

Overlay addressing and the built-in DHCP responder. Without this table, VMs
get no address from xdebbied and another DHCP server must serve the bridge.
See [Addresses and DHCP](../network/addresses-and-dhcp.md).

| Key | Type | Default | Description |
|---|---|---|---|
| `cidr` | CIDR | required | The overlay subnet, the same on every box. Bootstrap uses `--subnet` (default `10.23.0.0/16`). |
| `gateway` | IPv4 | first host of `cidr` | Router (option 3) handed to VMs. Bootstrap writes this box's bridge, `10.23.BOX.1`. |
| `dns` | array of IPv4 | `[gateway]` | Resolvers (option 6) handed to VMs. |
| `pool_start` | IPv4 | network address + 16 | First address the allocator hands out. Bootstrap writes `10.23.BOX.2`. |
| `pool_end` | IPv4 | last host of `cidr` | Last address the allocator hands out. Bootstrap writes `10.23.BOX.254`. |
| `reserved` | array of IPv4 | `[]` | Never allocate these. |
| `reserve_from_dns` | array of `{ zone, server }` | `[]` | Zones whose A records inside `cidr` are reserved too. `server` is an IP address (optionally `IP:PORT`) that allows zone transfers. Refreshed every minute. |
| `upstream_dns` | array of IPv4 | `[]` | Where VM DNS queries to the gateway are forwarded (DNAT to the first entry). Bootstrap writes the first box's resolver; joining boxes copy it. Empty: VMs have no DNS. |
| `mesh_routed` | boolean | `false` | Let routed mesh peers (xdj without Geneve) open connections to this box's VMs. `PUT /v1/settings` changes it at runtime. |
| `storage_cidr` | CIDR | unset | The storage network for `[[hosts]] storage_bridge`, for example `10.24.0.0/16`. Same prefix length as `cidr`. |
| `dhcp` | boolean | `true` | Run the DHCP responder. `false` allocates addresses only. |
| `bridge` | string | the mesh bridge, else the first host's bridge | Bridge the responder listens on. |
| `lease_secs` | integer | `86400` | Lease time offered. Renewals are no-ops, so long is fine. |
| `domain` | string | unset | Domain name (option 15). |
| `ntp` | array of IPv4 | `[]` | NTP servers (option 42), offered when not empty. |
| `leader_only` | boolean | `false` | Answer DHCP only while this box is the elected leader. |

## `[mesh]`

The Geneve overlay between boxes. On by default: a config with no `[mesh]`
table behaves as if it had one with these defaults, with `bridge` taken from
the first `[[hosts]]` entry. See [The mesh](../network/mesh.md).

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `true` | `false` turns the mesh off. |
| `box` | integer 0-253 | `0` | This box's id, unique in the cluster. Assigned by the CA box at enrolment. |
| `bridge` | string | the first `[[hosts]]` bridge, else `br-int` | Overlay bridge the tunnels join. |
| `vni` | integer | `23` | Geneve VNI, the same on every box. |
| `dstport` | integer | `6081` | Geneve UDP port. |
| `underlay` | string | `"wg-flock"` | Interface peers are reached through. |
| `allow_plain_underlay` | boolean | `false` | `true`: you provide the underlay and xdebbied does not manage WireGuard. |
| `mtu` | `"auto"` or integer | `"auto"` | Overlay MTU. `auto` is the underlay MTU minus 50. A fixed value must be at least 1280. |
| `fdb` | `"derived"` or `"learn"` | `"derived"` | `derived` installs static FDB entries from the MAC scheme; `learn` lets the bridge learn. |
| `firewall` | boolean | `true` | Install the `inet xdebbie_mesh` nftables table that drops Geneve arriving on any interface but the underlay. |
| `probe_interval_ms` | integer | `1000` | Peer probe interval. |
| `probe_timeout_ms` | integer | `800` | Upper bound on a probe's wait. |
| `probe_down_after` | integer | `3` | Failed probes before a peer is `Down`. |
| `probe_up_after` | integer | `3` | Good probes before a peer is `Up`. |
| `blackholes` | array of CIDRs | `["10.22.0.0/16", "10.23.0.0/16", "10.24.0.0/16"]` | Networks to blackhole at metric 4294967295, so traffic to them is dropped when their link goes away. `[]` turns it off. IPv4, no host bits set, prefix 1-32. See [Blackhole routes](../network/routes-and-firewall.md#blackhole-routes-for-the-mesh-networks). |

### `[mesh.wireguard]`

The managed WireGuard underlay. Used whenever `allow_plain_underlay` is
false, with these defaults if the table is absent.

| Key | Type | Default | Description |
|---|---|---|---|
| `listen_port` | integer | `51820` | WireGuard UDP port. |
| `address` | IPv4 | `10.22.BOX.1` | This box's underlay address. |
| `prefix` | integer | `16` | Prefix length of `address`. |
| `mtu` | integer | `1420` | MTU of the WireGuard link. |
| `keepalive` | integer | `25` | Persistent keepalive, seconds. `0` turns it off. |

### `[[mesh.peers]]`

One entry per other box.

| Key | Type | Default | Description |
|---|---|---|---|
| `box` | integer 0-253 | required | The peer's box id. |
| `pubkey` | string | required with managed WireGuard | The peer's WireGuard public key (`xdctl mesh wg` on that box). |
| `endpoint` | `HOST:PORT` | unset | Where to reach the peer's WireGuard. Omit for a peer behind NAT; it connects to you. |
| `remote` | IPv4 | `10.22.BOX.1` with managed WireGuard | The peer's underlay address. Required with `allow_plain_underlay`. |
| `probe` | IPv4 | `10.23.BOX.1` (`derived`), `remote` (`learn`) | Address probed to decide `Up` or `Down`. |

### `[[mesh.clients]]`

xdj clients: WireGuard-only peers. Needs the managed WireGuard underlay.

| Key | Type | Default | Description |
|---|---|---|---|
| `box` | integer 1-254 | required | The client's number, unique across this box, its peers and clients. |
| `pubkey` | string | required | The client's WireGuard public key (44 characters). |
| `name` | string | unset | A label: letters, digits, `.`, `_`, `-`; not all digits; unique. |
| `endpoint` | `HOST:PORT` | unset | Usually omitted: clients connect to the hub. |

## `[clients]`

What this box tells its xdj clients. Served publicly at
`GET /v1/client-config`, so put nothing secret here.

| Key | Type | Default | Description |
|---|---|---|---|
| `search` | array of strings | `[]` | DNS search domains, in order of preference. |
| `welcome` | string | `""` | A banner xdj prints once connected. Printable text, at most 4 KiB. |
| `dns` | array of tables | `[]` | `[[clients.dns]]` entries, below. |

### `[[clients.dns]]`

| Key | Type | Default | Description |
|---|---|---|---|
| `zone` | string | required | A DNS zone, for example `mesh.example.org`. |
| `server` | IPv4 | required | The resolver for names in `zone`. |

## `[[hosts]]`

Hypervisors to register at startup. Bootstrap writes one entry for this
box. Each key also accepts its camelCase spelling (`failureDomain`), which
is how the API spells it.

| Key | Type | Default | Bootstrap writes | Description |
|---|---|---|---|---|
| `name` | string | required | the hostname | Unique host name. |
| `uri` | string | required | `qemu:///system` | libvirt connection URI. |
| `failure_domain` | string | `"default"` | `"default"` | Rack, PDU or room. Cluster API spreads control planes across these. |
| `control_plane_eligible` | boolean | `true` | `true` | May run control-plane VMs. |
| `labels` | table of strings | `{}` | not written | Matched by a VM's `placement.requiredLabels`. |
| `storage_pool` | string | `"default"` | `"xdebbie"` | libvirt storage pool for VM disks and seeds. |
| `bridge` | string | `"br0"` | `"virbr-xdebbie"` | Bridge VMs attach to. |
| `storage_bridge` | string | unset | not written | Second, isolated bridge for storage traffic (`virbr-abe`). Needs `[network] storage_cidr`. |
| `cpu_overcommit` | float | `4.0` | `2.0` | vCPUs per physical CPU. |
| `mem_overcommit` | float | `1.0` | `1.0` | Memory overcommit ratio. |
| `reserved_cpus` | integer | `2` | `2` | CPUs held back for the host. |
| `reserved_memory_mib` | integer | `4096` | `4096` | Memory held back for the host, MiB. |
| `abe` | URL | unset | not written | This host's ABE API, for example `https://10.22.0.1:443`. Needed for `storage = abe` VMs. |
| `abe_overcommit` | float | `1.0` | not written | Thin pools: promised bytes may reach pool size times this. |
| `abe_max_data_percent` | float | `85.0` | not written | Refuse new ABE volumes above this pool data fill. |
| `abe_max_metadata_percent` | float | `80.0` | not written | The same for thin metadata. |
| `cordoned` | boolean | `false` | not written | No new VMs. Usually set with `xdctl host cordon`. |
| `agent` | URL | unset | not written | API URL of the xdebbied on that host. Set by that daemon when it registers; do not set it by hand. |

The daemon fills `capacity`, `last_seen` and `abe_capacity` itself. They
are accepted in the file but overwritten.

## A minimal file

What bootstrap writes for the first box, without comments:

```toml
listen = "0.0.0.0:23443"
state_file = "/var/lib/xdebbie/state.json"
token_file = "/etc/xdebbie/token"
tls_cert = "/etc/xdebbie/api-cert.pem"
tls_key  = "/etc/xdebbie/api-key.pem"
policy = "spread"
reconcile_interval_secs = 5
host_stale_secs = 60
virsh = "virsh"
seed_command = ["cloud-localds", "--network-config={net}", "--vendor-data={vendor}", "{out}", "{user}", "{meta}"]
enable_discovery = false
multicast_addr = "239.0.0.42:23442"
advertise_url = "https://box0.example.org:23443"

[network]
cidr = "10.23.0.0/16"
gateway = "10.23.0.1"
dns = ["10.23.0.1"]
upstream_dns = ["192.0.2.53"]
pool_start = "10.23.0.2"
pool_end = "10.23.0.254"
reserved = []

[mesh]
enabled = true
box = 0
bridge = "virbr-xdebbie"
vni = 23
dstport = 6081
underlay = "wg-flock"
allow_plain_underlay = false
mtu = "auto"
fdb = "derived"
firewall = true

[mesh.wireguard]
listen_port = 51820
mtu = 1420
keepalive = 25

[[hosts]]
name = "box0"
uri = "qemu:///system"
failure_domain = "default"
bridge = "virbr-xdebbie"
storage_pool = "xdebbie"
cpu_overcommit = 2.0
mem_overcommit = 1.0
reserved_cpus = 2
reserved_memory_mib = 4096
control_plane_eligible = true
```

`tests/lint-config-doc.sh` checks that every key the daemon accepts appears
on this page.
