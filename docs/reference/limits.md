# Limits and max values

Every hard limit and timing in xdebbied, xdctl and capx, where it is set,
and what happens when you reach it; then what xdebbie does not do yet.

Paths are under `crates/`. A limit that comes from elsewhere (the kernel,
libvirt, QEMU, a protocol) is marked *inherited*. The configuration keys
themselves are in [Configuration](configuration.md).

## Boxes, overlay and mesh

| Limit | Value | Where | When reached |
|---|---|---|---|
| Box ids | 0-253: at most 254 boxes | `MAX_BOX` (`xdebbied/src/enroll.rs`), `mesh/validate.rs` | config refused; enrolment refuses when none is free |
| Client numbers (xdj) | 1-254, not a box's or peer's | `mesh/validate.rs` | refused; boxes and clients share the space |
| Boxes per overlay | one `/24` each: `2^(24 - prefix)`; a `/16` holds 256 (ids stop at 253) | `capacity()` (`bootstrap/addr.rs`) | bootstrap fails: *has room for N box(es)*; longer than `/24` fails outright |
| VM addresses per box | `.2`-`.254` of the box's `/24`: 253 | `box_block()` (`bootstrap/addr.rs`) | no address, no VM |
| Default pool without bootstrap's layout | network + 16 to the last host | `POOL_SKIP` (`dhcp/model.rs`) | the same |
| `[network] cidr` prefix | 8-30 | `Cidr::new` (`dhcp/model.rs`) | config refused |
| `storage_cidr` | same prefix as `cidr`, not `cidr` itself | `dhcp/model.rs` | config refused |
| Geneve VNI | 1-16,777,215 (*inherited*: RFC 8926); default 23 | `DEFAULT_VNI` (`mesh/model.rs`) | refused |
| Geneve port | default 6081 | `DEFAULT_DSTPORT` | 0 refused |
| Geneve overhead | 50 bytes | `GENEVE_OVERHEAD` | `mtu = "auto"` is underlay − 50 |
| Overlay MTU | at least 1280, at most underlay − 50 | `MIN_MTU`, `mesh/validate.rs` | refused |
| WireGuard MTU | default 1420; at least 1330 | `DEFAULT_WG_MTU` | refused |
| WireGuard prefix | default 16; 8-30 | `mesh/validate.rs` | refused |
| WireGuard port, keepalive | 51820, 25 s | `DEFAULT_WG_PORT`, `d_wg_keepalive()` | |
| Interface names | 1-15 characters (*inherited*: `IFNAMSIZ`) | `mesh/validate.rs` | refused |
| Probes | every 1000 ms, wait 800 ms; down after 3 misses, up after 3 replies | `mesh/model.rs` | timeout above interval, or 0, refused |
| Adaptive probe wait | 15 × smoothed RTT, at least 250 ms, at most the configured wait | `ADAPT_RTTS`, `ADAPT_FLOOR` (`mesh/probe.rs`) | a slower echo counts as a miss |
| First probe | 1 s after start | `FIRST_PROBE_DELAY` | |
| Mesh reconcile | every 5 s | `RECONCILE_PERIOD` (`mesh/mod.rs`) | |
| `ip`, `bridge`, `wg` commands | 5 s each | `CMD_TIMEOUT` (`mesh/kernel.rs`) | killed, counted failed, retried next pass |
| Blackhole metric | 4294967295 | `BLACKHOLE_METRIC` (`mesh/kernel.rs`) | |
| Client routes | `proto 230` | `ROUTE_PROTO` (`mesh/routes.rs`) | |

## Discovery, election and enrolment

| Limit | Value | Where | When reached |
|---|---|---|---|
| Discovery group | `239.0.0.42:23442` | `DEFAULT_MULTICAST_ADDR` (`discovery/multicast.rs`) | |
| Datagram | 65,536 bytes | `MAX_PACKET_SIZE` | send fails |
| Announce, heartbeat | every 5 s, every 1 s | `discovery/engine.rs` | |
| Peer timeout | 15 s of silence | `PEER_TIMEOUT` | peer dropped |
| Heartbeat timeout | 5 s | `HEARTBEAT_TIMEOUT` | followers start an election |
| Election timeout | 10 s | `ELECTION_TIMEOUT` | the candidate wins if nobody better proposed |
| Votes to win | `n / 2 + 1` of the boxes heard | `election/state.rs` | |
| Clock skew | 30 s | `MAX_SKEW` (`discovery/wire.rs`) | datagrams dropped; run NTP |
| Replay memory | 75 s | `REPLAY_MEMORY` | |
| Bootstrap's listen | 20 s, plus 3 s once something is heard | `DEFAULT_LISTEN_SECS`, `SETTLE` (`bootstrap/discover.rs`) | nothing heard: a new cluster |
| Mirror of the leader | every 5 s | `mirror.rs` | a new leader's copy is up to 5 s old |
| Host registration | every 5 s; resent every 60 s | `registrar.rs` | |
| Pending enrolments | 64 | `MAX_PENDING` (`enroll.rs`) | new requests refused |
| Pending xdj request | 15 min | `CLIENT_PENDING_MINUTES` | denied |
| xdj client name | 1-63 of `[A-Za-z0-9._-]` | `enroll.rs`, `mesh/validate.rs` | refused |
| `bootstrap --join` poll | every 3 s; a reminder every minute | `POLL`, `REMIND_EVERY` (`bootstrap/join.rs`) | |
| `xdctl` leader wait | 20 s | `LEADER_WAIT_SECS` (`xdctl/src/main.rs`) | gives up |
| Token rotation sweep | every 3 s while rotating, else 60 s | `token.rs` | |

## API and TLS

| Limit | Value | Where | When reached |
|---|---|---|---|
| API port | 23443 | `DEFAULT_PORT` (`config.rs`) | |
| Request body | 2 MiB (*inherited*: axum's default) | | 413 |
| Body relayed to the leader | 16 MiB | `FORWARD_BODY_LIMIT` (`api/mod.rs`) | the forward fails |
| Forwarding hops | 1 | `X-Xdebbie-Forwarded-By` | a second hop answers 503 |
| Outgoing connect timeout | 10 s | `Config::http_client` | call fails |
| Non-TLS first byte | 5 s | `FIRST_BYTE_WAIT` (`tls.rs`) | a plain-text client gets a one-line reply and is closed |
| CA certificate | 10 years | `CA_DAYS` (`bootstrap/api_tls.rs`) | every certificate stops verifying |
| Box certificate | 2 years | `SERVER_DAYS` | peers and clients refuse the box |
| xdj client certificate | 60 min | `CLIENT_MINUTES` | xdj renews before then |
| Welcome banner | 4096 bytes | `WELCOME_MAX` (`client_config.rs`) | `PUT` refused |
| DNS names in client config | 63 bytes a label, 253 in all (*inherited*: RFC 1035) | `client_config.rs` | `PUT` refused |
| `/v1/qos` cache; one abe's answer | 2 s; 2 s | `QOS_CACHE`, `QOS_ABE_TIMEOUT` | that abe's volumes are left out |

## VMs and hosts

| Limit | Value | Where | When reached |
|---|---|---|---|
| VM name | DNS label: 1-63 of `[a-z0-9-]`, no `-` at either end | `api/mod.rs` | 400 |
| VM size | vCPUs above 0, memory at least 256 MiB, disk above 0; no maximum | `api/mod.rs` | 400 |
| LVM root disk of a `k8s` image | at least 25 GiB | `MIN_ROOT_DISK_GIB` (`lvm.rs`) | rounded up |
| Schedulable vCPUs | `(cpus − reserved_cpus) × cpu_overcommit` | `allocatable()` (`xdebbie-core/src/model.rs`) | `cpu` |
| Schedulable memory | `(memory − reserved_memory_mib) × mem_overcommit` | the same | `memory` |
| Host contact | stale after 60 s | `host_stale_secs` | unhealthy: no new VMs |
| Reconcile loop | every 5 s | `reconcile_interval_secs` | |
| Hot-plug PCIe root ports | 16 per VM | `HOTPLUG_ROOT_PORTS` (`libvirt.rs`) | *inherited* from q35: root ports cannot be hot-plugged |
| Volumes per VM | 255 SCSI targets | `SCSI_MAX_TARGET` (*inherited*: virtio-scsi) | attach fails |
| virtio-scsi queues | 4 | `SCSI_QUEUES` | |
| No slot for the SCSI controller | retried after 600 s | `no_scsi_slot::FOR` | the VM is skipped 10 min |
| Disk serial | 20 bytes virtio-blk, 36 scsi-hd (*inherited*) | `VIRTIO_SERIAL_LEN`, `SCSI_SERIAL_LEN` | the id is cut to fit |
| Failed attach retry | 5 s, doubling to 300 s | `attach_backoff` (`reconcile.rs`) | forgotten on success or restart |
| Live migration ports | TCP 49152-49215 (*inherited*: libvirt) | `qemu.conf` | migration fails while closed |

## Storage

| Limit | Value | Where | When reached |
|---|---|---|---|
| abe pool data fill | 85% | `abe_max_data_percent` | `abe pool full` |
| abe pool metadata fill | 80% | `abe_max_metadata_percent` | the same; a full metadata LV breaks the pool |
| Thin promises | sum of sizes ≤ pool × `abe_overcommit` (1.0) | `AbeCapacity::limit()` | `abe capacity` |
| Volume name | 1-253 bytes; size above 0 | `create_volume` (`api/mod.rs`) | 400 |
| One abe call | 120 s | `abe::TIMEOUT` | retried |
| abe `/version` cache | 60 s | `FEATURE_TTL` (`abe.rs`) | |
| NVMe/TCP I/O queues | 4 per connect | `NVME_IO_QUEUES` | |
| NVMe/TCP queue depth | kernel default, 128 (*inherited*) | | requests wait in the guest |
| Controller loss | retry for ever (`--ctrl-loss-tmo -1`) | `abe.rs` | the guest stalls instead of erroring |
| abe export ports | 23000-23999 | `ABE_EXPORT_PORTS` (`bootstrap/firewall.rs`) | must match abe's range |

## DHCP and DNS

| Limit | Value | Where | When reached |
|---|---|---|---|
| Lease | 86,400 s; at least 60 | `DEFAULT_LEASE_SECS` (`dhcp/model.rs`) | below 60 refused; T1 half, T2 seven eighths |
| `domain` | 1-253 characters | `dhcp/model.rs` | refused |
| MACs remembered (`net seen`) | 1024 | `MAX_SEEN` (`dhcp/mod.rs`) | the oldest is forgotten |
| Socket rebind | after 5 s | `REBIND_DELAY` | |
| DNS reservations | read every 60 s; a transfer 10 s, 16 MiB, 4096 messages | `main.rs`, `dhcp/dnsres.rs` | the last good set stays |

## Cluster API (capx)

| Limit | Value | Where |
|---|---|---|
| Requeue | cluster 120 s ready, 15 s not; machine 60 s provisioned, 10 s not; 15 s after an error | `capx/src/controller.rs` |
| xdebbied call | 30 s | `capx/src/xdebbie.rs` |
| Replicas | 1 (no leader election) | |

## Which limits bite first

On a small cluster, usually in this order:

1. **Host memory.** With `mem_overcommit = 1.0` and 4 GiB reserved, a 64
   GiB box takes seven 8 GiB VMs.
2. **The abe pool's 85% and 80% fill**, and with `abe_overcommit = 1.0` the
   sum of volume sizes: thin volumes count at their full size.
3. **253 VMs from one pool** when discovery is on and one leader allocates
   every address ([Addresses](../network/addresses-and-dhcp.md#how-a-vm-gets-its-address)).
4. **64 pending enrolments**, only if something floods the endpoint.
5. **254 boxes** is far off for most sites.

## What xdebbie does not do yet

**Cluster**

- Discovery is off by default; each box then leads alone with its own
  store. Point every client at one box, or turn discovery on.
- Discovery needs one L2 segment with multicast.
- There is no replicated store; followers mirror the leader every 5
  seconds.
- Token rotation reaches only the boxes the leader knows through
  discovery.
- The CA lives on one box. Without it (or a backup of its key) no box can
  join and no certificate can be signed. There is no revocation.
- Mesh peers are added by hand on each box: enrolment exchanges
  certificates, not WireGuard keys.

**VMs**

- x86_64 only (q35, `host-passthrough`).
- VMs are immutable: to resize, re-create.
- `stop` has no timeout and no forced power-off; delete always forces.
- No dynamic DHCP pool, no DNS for VM names, no IPv6.

**Storage and migration**

- `local` disks stay on their host; a dead host's `local` VMs cannot start
  elsewhere.
- No live migration of `abe` VMs, nor of LVM-backed VMs between boxes.
- No garbage collection of abe base volumes.
- No disk encryption, and no IOPS caps in xdebbie (abe computes them,
  abe-csi enforces them).

**Mesh**

- `fdb = "derived"` does not cover VM MACs; use `learn` when VMs on
  different boxes must talk.

**Cluster API**

- No MachinePool, no IPAM contract, no in-place updates, no load balancer
  (use kube-vip), one capx replica. `Dockerfile` and `deploy/capf.yaml`
  still use the pre-release name.

**Observability**

- No metrics endpoint. The mesh keeps a Prometheus-style snapshot in
  `GET /v1/mesh` (`status.metrics`) until there is one.
