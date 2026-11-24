# Max values: xdebbied's limits

Every hard-coded or configurable limit in xdebbied (and xdctl and capx), with
its default, where it is set, and what happens when it is reached. Values are
read from the source on `main`; paths are relative to `crates/`. A limit that
comes from somewhere else (the kernel, libvirt, QEMU, WireGuard, a protocol
spec, a library) is marked **inherited** with its source.

For what xdebbie does not do at all, see [Limits and known gaps](limits.md).
For the configuration keys themselves, see
[Configuration](reference/configuration.md).

## Boxes, overlay and mesh

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| Mesh box ids | 0–253, so at most **254 boxes** per overlay | `MAX_BOX` (`xdebbied/src/enroll.rs:75`); checked in `mesh/validate.rs:44` (`box`) and `:174` (`peers[].box`) | Config is rejected: `must be 0–253`. Enrolment picks the lowest free id and refuses when none is left. |
| Mesh client ids (xdj) | 1–254, not the local box id and not a peer's id | `mesh/validate.rs:281` | Config is rejected: `must be 1–254`. Clients share the box-id space, so boxes plus clients stay under 255. |
| Overlay subnet | default `10.23.0.0/16` | `DEFAULT_NETWORK_SUBNET` (`bootstrap/mod.rs:35`); `[network] cidr` | — |
| Boxes per overlay subnet | one /24 per box: `2^(24 − prefix)`; a /16 holds 256 blocks (box ids stop at 253) | `capacity()` (`bootstrap/addr.rs:38`) | Bootstrap fails: `overlay … has room for N box(es), not box M`. A prefix longer than /24 fails outright. |
| VM addresses per box (bootstrap layout) | `.2`–`.254` of the box's /24: **253 VMs** per box; `.1` is the bridge | `box_block()` (`bootstrap/addr.rs:46`) | Address allocation fails and the VM is not created. |
| VM addresses, default pool (no bootstrap layout) | `network + 16` … last host of `cidr` | `POOL_SKIP = 16` (`dhcp/model.rs:16`); `[network] pool_start`, `pool_end`, `reserved`, `reserve_from_dns` | Same: no free address, no VM. |
| `[network] cidr` prefix length | 8–30 | `Cidr::new` (`dhcp/model.rs:28`) | Config is rejected. |
| `[network] storage_cidr` | same prefix length as `cidr`, not `cidr` itself | `dhcp/model.rs` `validate()` | Config is rejected. |
| WireGuard underlay | `10.22.<box>.1`, prefix /16 | `wireguard.address`, `d_wg_prefix()` = 16 (`mesh/model.rs:222`) | — |
| Geneve VNI | 1–16,777,215 (24 bits, **inherited**: RFC 8926); default 23 | `DEFAULT_VNI` (`mesh/model.rs:12`); check `mesh/validate.rs:59` | Config is rejected. |
| Geneve UDP port | default 6081 (IANA) | `DEFAULT_DSTPORT` (`mesh/model.rs:14`) | 0 is rejected. |
| Geneve overhead | 50 bytes | `GENEVE_OVERHEAD` (`mesh/model.rs:16`) | `mtu = "auto"` is the underlay MTU − 50. |
| Overlay MTU | at least 1280; at most underlay MTU − 50 | `MIN_MTU` (`mesh/model.rs:18`), `auto_mtu()` (`:393`), checks `mesh/validate.rs:94` | Config is rejected. |
| WireGuard MTU | default 1420; at least 1330 (1280 + 50) | `DEFAULT_WG_MTU` (`mesh/model.rs:26`); `mesh/validate.rs:146` | Config is rejected. |
| WireGuard port | default 51820 | `DEFAULT_WG_PORT` (`mesh/model.rs:24`) | 0 is rejected. |
| WireGuard keepalive | 25 s | `d_wg_keepalive()` (`mesh/model.rs:228`) | — |
| Interface names (bridge, underlay) | 1–15 characters (**inherited**: Linux `IFNAMSIZ` = 16 with the NUL) | `mesh/validate.rs:51`, `:84` | Config is rejected. |
| Mesh probe | every 1000 ms, timeout 800 ms; down after 3 misses, up after 3 replies | `d_probe_interval`, `d_probe_timeout`, `d_three` (`mesh/model.rs:100–106`); `[mesh.probe]` | timeout > interval, or 0, is rejected. |
| Probe timeout, adaptive | 15 × smoothed RTT, at least 250 ms, at most the configured timeout | `ADAPT_RTTS`, `ADAPT_FLOOR` (`mesh/probe.rs:115`) | A slower echo counts as a miss. |
| First probe | 1 s after the prober starts | `FIRST_PROBE_DELAY` (`mesh/probe.rs:119`) | — |
| Mesh reconcile | every 5 s | `RECONCILE_PERIOD` (`mesh/mod.rs:76`) | — |
| Kernel commands (`ip`, `bridge`, `wg`) | 5 s each | `CMD_TIMEOUT` (`mesh/kernel.rs:20`) | The command is killed and counted as failed; the next pass retries. |

## Discovery, election and enrolment

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| Discovery group | `239.0.0.42:23442` | `DEFAULT_MULTICAST_ADDR` (`discovery/multicast.rs:6`); `multicast_addr` | — |
| Discovery datagram | 65,536 bytes | `MAX_PACKET_SIZE` (`discovery/multicast.rs:7`) | Send fails: `Message too large`. |
| Announce / heartbeat | every 5 s / every 1 s | `ANNOUNCE_INTERVAL`, `HEARTBEAT_INTERVAL` (`discovery/engine.rs:22`) | — |
| Peer timeout | 15 s of silence | `PEER_TIMEOUT` (`discovery/engine.rs:24`) | The peer is dropped from the table. |
| Election timeout | 10 s in one election state | `ELECTION_TIMEOUT` (`discovery/engine.rs:25`; used in `election/state.rs:258`) | The box gives up on that round and campaigns again. |
| Heartbeat timeout | 5 s without the leader's heartbeat | `HEARTBEAT_TIMEOUT` (`discovery/engine.rs:26`) | Followers start an election. |
| Votes to win | majority: `n / 2 + 1` of the boxes heard | `election/state.rs:230` | No leader until a majority answers. |
| Clock skew on signed discovery | 30 s | `MAX_SKEW` (`discovery/wire.rs:43`) | Datagrams outside it are dropped. Keep NTP running. |
| Replay memory | 2 × 30 + 15 = 75 s | `REPLAY_MEMORY` (`discovery/engine.rs:32`) | — |
| `bootstrap` listen time | 20 s, plus 3 s once something is heard | `DEFAULT_LISTEN_SECS`, `SETTLE` (`bootstrap/discover.rs:26`) | Nothing heard: the box starts a cluster of its own. |
| Follower mirror of the leader's store | every 5 s | `INTERVAL` (`mirror.rs:24`) | A new leader starts from a copy up to 5 s old. |
| Host registration with the leader | every 5 s; re-sent every 60 s even when unchanged | `INTERVAL`, `REFRESH` (`registrar.rs:27`) | — |
| Pending enrolments | **64** | `MAX_PENDING` (`enroll.rs:27`) | New requests are refused until some are decided. |
| Pending xdj client request | 15 min | `CLIENT_PENDING_MINUTES` (`enroll.rs:31`) | Denied automatically. |
| xdj client name | 1–63 characters of `[A-Za-z0-9._-]` | `client_name_ok()` (`enroll.rs:33`) | Request is refused. |
| `bootstrap --join` poll | every 3 s; a reminder every 20 polls (1 min) | `POLL`, `REMIND_EVERY` (`bootstrap/join.rs:29`) | Waits until an admin approves. |
| `xdctl` leader wait | 20 s | `LEADER_WAIT_SECS` (`xdctl/src/main.rs:485`) | `xdctl` gives up. |
| Token rotation sweep | every 3 s while rotating, else every 60 s | `ROTATING_INTERVAL`, `STEADY_INTERVAL` (`token.rs:40`) | — |

## API and TLS

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| API port | 23443 | `DEFAULT_PORT` (`xdebbied/src/config.rs:228`); `listen` | — |
| Request body (JSON handlers) | 2 MiB (**inherited**: axum 0.8 `DefaultBodyLimit`; xdebbied does not change it) | axum | 413 Payload Too Large. |
| Body a follower relays to the leader | 16 MiB | `FORWARD_BODY_LIMIT` (`api/mod.rs:56`) | The forward fails. |
| Forwarding hops | 1 (`x-xdebbie-forwarded-by`) | `api/mod.rs:52` | A second hop answers 503 (leadership moved mid-request). |
| Outgoing HTTP connect timeout | 10 s | `Config::http_client` (`config.rs:322`) | Call fails. |
| Non-TLS first byte | 5 s | `FIRST_BYTE_WAIT` (`tls.rs:193`) | A plain-text client gets a one-line reply and is closed. |
| CA certificate lifetime | 10 years (3650 days) | `CA_DAYS` (`bootstrap/api_tls.rs:37`) | Every box and client cert stops verifying. |
| Box server certificate | 2 years (730 days) | `SERVER_DAYS` (`bootstrap/api_tls.rs:38`) | Peers and clients refuse the box. |
| xdj client certificate | 60 min | `CLIENT_MINUTES` (`bootstrap/api_tls.rs:43`) | xdj renews it before then. |
| `/v1/client-config` welcome banner | 4096 bytes | `WELCOME_MAX` (`client_config.rs:37`) | `PUT` is rejected. |
| DNS names in client-config | 63 bytes a label, 253 in all (**inherited**: RFC 1035) | `client_config.rs` | `PUT` is rejected. |
| `/v1/qos` cache | 2 s | `QOS_CACHE` (`api/mod.rs:757`) | — |
| One abe's `/qos` answer | 2 s | `QOS_ABE_TIMEOUT` (`api/mod.rs:759`) | That abe's volumes are left out (unshaped). |

## VMs and hosts

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| VM name | DNS-1123 label: 1–63 of `[a-z0-9-]`, no leading or trailing `-` | `validate()` (`api/mod.rs:1050`) | 400. |
| VM size, minimum | vCPUs > 0, memory ≥ 256 MiB, disk > 0 GiB; no maximum in xdebbied | `api/mod.rs:1061` | 400. The scheduler then needs a host with room. |
| Root disk of a `k8s` image | at least 25 GiB | `MIN_ROOT_DISK_GIB` (`lvm.rs:22`) | Smaller requests are rounded up. |
| Schedulable vCPUs per host | `(cpus − reserved_cpus) × cpu_overcommit` | `allocatable()` (`xdebbie-core/src/model.rs`); `reserved_cpus` = 2, `cpu_overcommit` = 4.0 by default (`:120–127`) | Scheduler rejects the host: `cpu`. |
| Schedulable memory per host | `(memory − reserved_memory_mib) × mem_overcommit` | `reserved_memory_mib` = 4096, `mem_overcommit` = 1.0 by default | Scheduler rejects the host: `memory`. |
| Host contact | stale after 60 s | `host_stale_secs` (`config.rs:155`) | Host counts as unhealthy; no new VMs there. |
| Reconcile loop | every 5 s | `reconcile_interval_secs` (`config.rs:152`) | — |
| Hot-plug PCIe root ports per VM | 16 | `HOTPLUG_ROOT_PORTS` (`libvirt.rs:684`) | Root ports cannot be hot-plugged (**inherited**: QEMU/q35); a VM defined with fewer has fewer. |
| abe volumes per VM (virtio-scsi targets) | **255** (`sdb` …) | `SCSI_MAX_TARGET` (`libvirt.rs:703`); **inherited**: QEMU virtio-scsi max target 255 | The attach fails. |
| virtio-scsi queues | 4 | `SCSI_QUEUES` (`libvirt.rs:705`) | — |
| No room for the SCSI controller | retried after 600 s | `no_scsi_slot::FOR` (`libvirt.rs:937`) | The VM is skipped for 10 min. |
| Disk serial | 20 bytes virtio-blk, 36 scsi-hd (**inherited**: `VIRTIO_BLK_ID_BYTES`, QEMU `MAX_SERIAL_LEN`) | `VIRTIO_SERIAL_LEN`, `SCSI_SERIAL_LEN` (`xdebbie-core/src/model.rs:398`) | xdebbied uses the first 20 (or 36) characters of the volume id as the serial. |
| Failed attach retry | 5 s, doubling to 300 s | `attach_backoff` (`reconcile.rs:665`) | Forgotten on success or restart. |
| Live migration ports | TCP 49152–49215 (**inherited**: libvirt `migration_port_min/max`) | libvirt `qemu.conf` | Migration fails when they are closed. |

## Storage (abe on the host)

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| Pool data fill | 85% | `abe_max_data_percent` (`xdebbie-core/src/model.rs:132`) | No new abe volumes on that host: `abe pool full`. |
| Pool metadata fill | 80% | `abe_max_metadata_percent` (`:135`) | Same. A full metadata LV breaks the thin pool. |
| Thin provisioning | sum of volume sizes ≤ pool size × `abe_overcommit` (default 1.0) | `AbeCapacity::limit()` (`:178`) | Scheduler rejects the host: `abe capacity`. |
| Volume name (`POST /v1/volumes`) | 1–253 bytes; size > 0 | `create_volume` (`api/mod.rs:1381`) | 400. |
| One abe call | 120 s (a create waits for fio and the trim) | `abe::TIMEOUT` (`abe.rs:235`) | The call fails; the VM or volume is retried. |
| abe `/version` cache | 60 s | `FEATURE_TTL` (`abe.rs:30`) | — |
| NVMe/TCP I/O queues per connect | 4 (one TCP connection each) | `NVME_IO_QUEUES` (`abe.rs:483`) | — |
| NVMe/TCP queue depth | not set, so the kernel default: 128 (**inherited**: `NVMF_DEF_QUEUE_SIZE`, `drivers/nvme/host/fabrics.h`) | — | Requests wait in the guest. |
| Controller loss timeout | `-1`: retry for ever | `--ctrl-loss-tmo -1` (`abe.rs:531`) | The guest stalls instead of getting I/O errors. |
| abe export ports | TCP 23000–23999 opened by bootstrap's firewall | `ABE_EXPORT_PORTS` (`bootstrap/firewall.rs:101`) | Must match abe's `ABE_PORT_MIN`/`MAX`. |

## DHCP and DNS

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| Lease | default 86,400 s (1 day); at least 60 | `DEFAULT_LEASE_SECS` (`dhcp/model.rs:13`); check `:217` | Config is rejected below 60. T1 = lease / 2, T2 = lease × 7/8 (`dhcp/respond.rs:119`). |
| `[network] domain` | 1–253 characters | `dhcp/model.rs:221` | Config is rejected. |
| MACs remembered | 1024 | `MAX_SEEN` (`dhcp/mod.rs:39`) | The oldest is forgotten. |
| Socket rebind | after 5 s | `REBIND_DELAY` (`dhcp/mod.rs:38`) | — |
| `reserve_from_dns` zone transfer | 16 MiB, 4096 messages, 10 s | `MAX_TOTAL`, `MAX_MESSAGES` (`dhcp/dnsres.rs:228`); `axfr(…, 10 s)` | The transfer is refused; the last good list is kept. |
| DNS name in a transfer | 255 bytes (**inherited**: RFC 1035) | `dhcp/dnsres.rs:402` | The transfer is refused. |

## Cluster API (capx)

| Limit | Value | Where | When it is reached |
|---|---|---|---|
| Requeue | 120 s when ready, 15 s otherwise; 10 s while waiting on an owner | `capx/src/controller.rs:207`, `:52`, `:149`, `:284` | — |
| xdebbied API call | 30 s | `capx/src/xdebbie.rs:21` | Reconcile fails and requeues. |
| Replicas | 1 (no leader election) | [limits.md](limits.md) | — |

## Not in xdebbied

- **No disk encryption.** Nothing in xdebbied sets up LUKS.
- **No IOPS or bandwidth caps.** abe's IOPS arbiter computes them and
  abe-csi enforces them (cgroup `io.max`); xdebbied only relays them at
  `GET /v1/qos`. See abe's `docs/max-values.md`.
- **No metrics endpoint yet.** See [limits.md](limits.md#observability).

## Which limits bite first

On a small cluster, usually in this order:

1. **Host memory** (`reserved_memory_mib`, `mem_overcommit = 1.0`). A host
   with 64 GiB takes seven 8 GiB VMs.
2. **The abe pool's 85% / 80% fill** and, with `abe_overcommit = 1.0`, the
   sum of volume sizes. Thin volumes count at their full size.
3. **64 pending enrolments**, only if something floods the endpoint.
4. **253 VMs per box** and **254 boxes** are far off for most sites.
