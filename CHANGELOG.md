# Changelog

All notable changes to XDebbie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.0] - 2026-11-12

### Added
- **Token rotation.** `POST /v1/token/rotate` on the leader (`xdctl token
  rotate`) mints a new bearer token and keeps accepting the old one. The
  leader pushes the pair to every peer (`PUT /v1/token`, with the token the
  peer still knows) and confirms each with `GET /v1/token` using the new
  token; when every peer is confirmed the old token is retired everywhere,
  peers first, leader last. `GET /v1/token` / `xdctl token status` show
  fingerprints and each peer's state (`current`, `overlap`, `installing`,
  `stale`, `unreachable`). On disk: `token_file` always holds the current
  token, `token_file.previous` exists during a rotation (a restart resumes
  it), and `export XDEBBIE_TOKEN=` in `xdebbie_env` is rewritten. A slow
  sweep afterwards tidies peers still holding a retired token and reports
  ones that missed the rotation as `stale`. `TokenStatus`, `TokenNode`,
  `TokenSet`, `TokenRotated` in `xdebbie-core`.

## [0.10.0] - 2026-11-12

### Changed
- **The elected leader decides.** Only the leader runs the reconciler
  (scheduler, VM steps, volume attach); followers log `follower: reconciler
  idle, the leader decides` and wait. Cluster routes (`/v1/hosts`, `/v1/vms`,
  `/v1/volumes`, `/v1/failure-domains`, `/v1/schedule`) asked of a follower
  are forwarded to the leader verbatim and the reply relayed
  (`X-Xdebbie-Forwarded-By` marks the hop; a second hop answers 503). During
  an election followers answer 503 with `Retry-After: 2`; an unreachable
  leader is a 502 naming it. `/v1/leader`, `/v1/network`, `/v1/mesh*` and
  `/healthz` stay local. Standalone (discovery disabled or no multicast
  socket) is unchanged: the box is its own leader.
- `xdctl` talks to the leader: it asks `/v1/leader` on the configured URL
  (now accepts a comma-separated seed list) and uses `leader.api_url` for
  the command; an election in progress is waited out for up to 20 s. Older
  daemons without `/v1/leader` are used as-is.
- The DHCP gate reads the same leadership as everything else.
- Discovery binds the multicast socket with `SO_REUSEADDR`/`SO_REUSEPORT`
  and on the port of `multicast_addr` (it was hard-wired to 7442).

### Added
- `GET /v1/leader`: `{this, state, is_leader, leader, peers}` and
  `xdctl leader`.
- `advertise_url` in `xdebbied.toml`: the URL peers and clients use for this
  box. Default derives from the listen address; `0.0.0.0` is replaced by
  the default-route interface IP instead of being announced as-is.
- `LeaderStatus`/`LeaderRef` in `xdebbie-core`.

### Removed
- Unused discovery leftovers (`PeerInfo.capacity`, `PeerTable::remove`,
  `highest_score`, `DiscoveryMessage::sequence`, `ElectionStateMachine::score`).

## [0.9.0] - 2026-11-11

### Added
- **Standalone volumes** (`/v1/volumes`, `xdctl volume`): ABE volumes with
  their own lifecycle, hot-plugged into a VM as a virtio disk. Create and
  delete talk to ABE synchronously (the caller needs the id ABE assigns);
  attach and detach are reconciled (`attachTo` wish, `attached {vm, host,
  target}` observation, `message`). The host does `nvme connect` and `virsh
  attach-device --persistent` with the volume id as the virtio serial, so
  the guest finds it at `/dev/disk/by-id/virtio-<first 20 chars of id>`
  and never needs a route to ABE. Re-attached every tick while wanted, so
  host reboots and recreated domains heal; a deleted VM just lets go of its
  volumes. ABE's `bytes` and fio `perf` are carried in the record. This is
  the path `abe-csi` uses from inside Kubernetes VMs (3× faster sequential
  reads than an in-guest NVMe/TCP initiator in our tests).
- `DiskRole::Volume` and `Volume`/`Attachment`/`VolumePerf` in
  `xdebbie-core`; `State.volumes` in the store (absent in old files).

## [0.8.6] - 2026-11-11

### Fixed
- `xdebbied.service` lets bootstrap write `/etc/ufw` and `/etc/default/ufw`
  (0.8.5's firewall step failed under `ProtectSystem` with
  "'/etc/default/ufw' is not writable" and ufw was never enabled). The unit
  now also carries `/etc/lvm` and `/dev` itself, replacing the hand-made
  `lvm-access.conf` drop-in hosts needed before.

## [0.8.5] - 2026-11-11

### Changed
- **ufw is the host firewall.** `xdebbied bootstrap` (the unit's
  `ExecStartPre`) replaces the `XDEBBIE-INPUT` iptables chain — which
  allowed all TCP from every local network, overlay included — with ufw:
  default deny inbound, `ufw allow` rules tagged `xdebbie …` for ssh, the
  API and libvirt TLS from local networks, WireGuard from anywhere, Geneve
  on the underlay interface only, discovery multicast, and DHCP on the
  bridge; then `ufw --force enable`. The legacy chain is removed if found.
  Rules are derived from `xdebbied.toml` (`listen`, `[mesh]`,
  `[mesh.wireguard]`, `multicast_addr`, `[network]`). `ufw` is a package
  dependency. See `docs/mesh.md` "Host firewall".
- `get_hostname()` prefers `hostname -f` (FQDN everywhere).

## [0.7.0] - 2026-11-11

### Added
- **ABE-backed VM disks** (`spec.storage = abe`, `docs/storage.md`). Each
  hypervisor runs an [ABE](https://github.com/brianmatheson/abe) NVMe/TCP
  exporter (`[[hosts]].abe = "http://…"`); a VM scheduled there gets its root
  (and optional data) volume from that ABE, attached on the host with
  `nvme connect … --ctrl-loss-tmo -1`, addressed by the stable
  `/dev/disk/by-id/nvme-uuid.<id>` link, root filled once from the base image
  with `qemu-img convert -n`. Volumes are recorded in `Vm.disks[]` (server,
  traddr, port, id, bytes, populated) *before* provisioning continues, so a
  failure or crash never allocates twice; `start` re-attaches after a host
  reboot; delete disconnects and removes every volume. Local storage is
  unchanged and remains the default.
- `xdctl vm create --storage abe`, `xdctl host add --abe URL`; CAPX
  `XDebbie8sMachineTemplate.spec.template.spec.storage` and `dataDiskGib`.
- Config: `nvme`, `qemu_img`, `nvme_sys` (binary/sysfs paths, for tests).
- `tests/abe/run.sh`: unprivileged end-to-end with the real daemon, a fake
  ABE API, stub nvme/qemu-img/virsh and a tmpfs over `/dev/disk/by-id`
  (10 checks incl. retry-without-leak and reboot re-attach).

### Changed
- `Hypervisor` trait gains `prepare_disks(host, vm, &mut disks)` (default
  no-op) and `Patch.disks`; the reconciler persists partial disk allocation
  even when provisioning fails.
- Live migration of `storage = abe` VMs is refused with a clear message
  (attaching on a remote host is the next step).

### Requires
- abe ≥ 0.2.0 on the hosts (`device_uuid`, `traddr` in replies, `raw=true`).

## [0.6.0] - 2026-11-11

### Added
- **Overlay addressing + built-in DHCP responder** (`[network]`,
  `docs/network.md`). The scheduler allocates each VM an IPv4 address in the
  same store transaction that creates it (`vm.ip`, first `InternalIP`, shown
  by `xdctl vm ls` before first boot; pin with `spec.ip` / `xdctl vm create
  --ip`). A small DHCPv4 responder bound to the bridge (`SO_BINDTODEVICE`,
  udp/67) answers *only* for MACs found in the VM store with exactly the
  stored address — OFFER/ACK/NAK/INFORM, unknown MACs ignored — and hands out
  mask, router, DNS, hostname, domain, lease, and **option 26 = the mesh MTU**
  when the bridge is a mesh overlay. No lease database: the (MAC, IP) pair is
  part of the VM record and lives as long as it does. `leader_only` gates
  answering on the election for the day control planes share state.
- Existing VMs are **adopted** on startup: a VM that already reported an
  `InternalIP` in `cidr` keeps it, others are allocated.
- `GET /v1/network`, `xdctl net show` (config, bound/serving/standby, counters)
  and `xdctl net seen` (MACs heard and what they were told).
- Responder rebinds by itself when the bridge is recreated (libvirt
  `net-destroy`/`net-start`) or when udp/67 becomes free.
- `tests/dhcp/run.sh`: unprivileged netns end-to-end with the real daemon and
  busybox `udhcpc` (13 checks: allocation/pinning/conflicts, real client
  bound with all options, stranger ignored, stale REQUEST → NAK, counters,
  address reuse after delete, rebind after bridge recreate).

### Changed
- `xdebbied bootstrap` creates the libvirt network **without** a dnsmasq DHCP
  range and writes an active `[network]`; when adopting a pre-existing
  network the block is emitted commented out.
- Reconcile keeps the allocated address first in `vm.addresses` whatever the
  guest agent / ARP table reports.

## [0.5.2] - 2026-11-11

### Fixed
- **WireGuard key unreadable under AppArmor.** Debian's `wireguard-tools` ships
  an AppArmor profile for `/usr/bin/wg` that only permits key files under
  `/etc/wireguard`, so `wg set <dev> private-key /var/lib/xdebbie/wg.key`
  failed every tick with `fopen: Permission denied` (even as root) and the
  underlay never came up. xdebbied now reads the key itself and feeds it to
  `wg set … private-key /dev/stdin` over a pipe, which AppArmor does not
  path-mediate; the file path is kept only as a fallback.
- The .deb also ships `/etc/apparmor.d/local/wg` (conffile) allowing
  `/var/lib/xdebbie/wg.key r,` and reloads the `wg` profile in postinst so
  manual `wg set … private-key /var/lib/xdebbie/wg.key` works for operators.

## [0.5.1] - 2026-11-11

### Changed
- **STP is forced off on the mesh bridge.** The mesh is loop-free by
  construction (isolated tunnel ports), and BPDUs leaking across Geneve
  tunnels would only make STP block ports or storm topology changes. The
  planner now emits `set_bridge_stp_off` whenever the bridge reports
  `stp_state 1` (libvirt turns it on by default; the next tick turns it
  back off), and `BridgeStpEnabled` only remains set if that write failed.
  Bootstrap creates the libvirt network with `stp='off'`.

### Fixed
- `xdctl … | head` no longer panics with "Broken pipe" (SIGPIPE restored to
  its default disposition).

## [0.5.0] - 2026-11-11

### Added
- **Managed WireGuard underlay for the mesh.** With `[mesh.wireguard]` (or
  `spec.wireguard` over the API) `xdebbied` creates the underlay interface
  named by `underlay` as a WireGuard link, keyed from
  `/var/lib/xdebbie/wg.key` (generated once, 0600), addressed
  `10.22.<box>.1/16`, and keeps one WireGuard peer per mesh peer
  (`allowed-ips <remote>/32`, `persistent-keepalive`). Geneve packets between
  boxes are therefore always encrypted and authenticated; a peer's `remote`
  is derived (`10.22.<box>.1`) so enrolment is just the peer's `pubkey` and
  `endpoint`. Same discipline as the Geneve layer: ownership by name + alias
  + kind, derived from kernel state each tick, zero writes on a converged
  restart, foreign links never touched (`WireguardConflict` blocks instead).
  `status.wireguard` reports this box's public key, port, address and
  per-peer handshake age / byte counters.
- `xdctl mesh wg` prints the local WireGuard identity and the exact
  `xdctl mesh peer add … --pubkey … --endpoint …` line to run on other boxes;
  `xdctl mesh peer add` gains `--pubkey`/`--endpoint` and makes `REMOTE`
  optional.
- Harness test M14: keys, enrolment, encrypted carrier (nft counters prove
  no plaintext Geneve crosses the carrier), silent restart, peer removal,
  teardown.
- Package now depends on `wireguard-tools`; `/etc/modules-load.d/xdebbie-mesh.conf`
  also loads `wireguard`.

### Changed
- `peers[].remote` is optional in the API/TOML when `wireguard` is set
  (required otherwise; validation says so).

## [0.4.1] - 2026-11-10

### Fixed
- **Bootstrap created the libvirt network with the subnet's network address
  as its `<ip address>`** (`192.168.100.0` instead of `192.168.100.1`), so the
  host had no usable gateway on the bridge. `ensure_network` now derives the
  first host address and the netmask from the CIDR for any prefix 8–30.
- **Bootstrap continued after `systemctl start libvirtd` failed** and then
  died three steps later with "failed to connect socket". It now probes the
  URI (`virsh uri`); if nothing answers it stops immediately and prints
  `systemctl status libvirtd` so the real cause (e.g. a `243/CREDENTIALS`
  TPM-sealed secret-driver key on Debian testing) is visible.

## [0.4.0-1] - 2026-11-10

### Added
- **Geneve overlay mesh (X12).** New `mesh` module in `xdebbied` stitches the
  overlay bridge of every host into one L2 segment: one `gnv-b<P>` Geneve
  tunnel per peer (same VNI/dstport, different remote) enslaved to the bridge
  with `isolated on`; `derived` mode pins 17 static FDB entries per peer and
  disables learning/flooding on tunnel ports; MTU = underlay − 50 applied to
  tunnels, bridge and new VM NICs; `nft table inet xdebbie_mesh` drops the
  Geneve port on non-underlay interfaces; in-process ICMP probes with
  hysteresis drive per-peer `Up/Down/Error` and `Ready/Degraded` conditions.
  Reconciliation is derived from kernel state (zero writes on a converged
  restart) and only ever touches links named `gnv-b*` carrying the
  `xdebbie-mesh/v1` alias. Configured via a `[mesh]` TOML section (validated
  at startup) or the new `/v1/mesh` API (`GET`/`PUT` with `If-Match`/`DELETE`,
  `/v1/mesh/peers/{box}`, `POST /v1/mesh/reconcile`), persisted in
  `/var/lib/xdebbie/mesh.json`; API state shadows TOML.
- `xdctl mesh ls|get|apply|peer add|peer rm|delete|reconcile`.
- `xdebbied serve --mesh-only` runs only the mesh manager and its routes.
- `tests/mesh/`: 3-host network-namespace harness and M1–M13 acceptance
  tests (run unprivileged under `unshare -Urnm`).
- Package ships `/etc/modules-load.d/xdebbie-mesh.conf` (`geneve`) and now
  depends on `iproute2` and `nftables`. The generated `xdebbied.toml` carries
  a commented-out `[mesh]` section; the mesh is **off by default**.

## [Unreleased] - 2026-11-08

### Added
- **Guest fstrim on every boot via NoCloud vendor-data.** Seeds now carry a
  vendor-data script (run once by cloud-init `scripts-vendor`, so it never
  merges with the caller's user-data) that installs `xdebbie-fstrim.service`
  -- a oneshot `fstrim -av` after `local-fs.target` -- and enables the distro
  `fstrim.timer`. Freed blocks go back to the thin pool instead of pinning
  allocation at the high-water mark. `seed_command` gains a `{vendor}`
  placeholder; configs without it get `--vendor-data` injected automatically
  when the builder is `cloud-localds`.

### Changed
- **LVM root disks honour `diskGib`** with a 25 GiB floor. Previously a thin
  snapshot silently inherited the base image's size (e.g. 10 GiB) regardless
  of what was requested. Now `create_root_disk` runs `lvextend` after the
  snapshot, and grows existing undersized root LVs on reconcile. The guest's
  cloud-init growpart/resizefs picks up the space on next boot.

### Fixed
- **VMs not coming back after host reboot**: LVM thin pool (and every VM disk
  in it) stayed inactive after reboot because `thin-provisioning-tools`
  (`thin_check`) was not installed, so libvirt autostart failed for all VMs.
  - `lvm2` and `thin-provisioning-tools` are now hard dependencies (deb
    `Depends:` and bootstrap `REQUIRED_PACKAGES`)
  - `LvmManager::ensure_activated()` runs `vgchange -ay -K` at startup and
    reports a clear, actionable error if the thin pool is still inactive
  - After storage activation, xdebbied starts any libvirt domains that are
    marked autostart but shut off (`virsh list --autostart --inactive`)
  - `xdebbied.service` now orders after `lvm2-monitor`, `libvirtd`, `virtqemud`
- **Bootstrap libvirtd check**: Changed from fatal error to warning when libvirtd.service fails
  - Allows bootstrap to continue when libvirt uses socket activation
  - Bootstrap now works on systems where libvirtd.service is broken but socket activation works
  - Emits warning: "libvirtd service not available (this is OK if using socket activation)"

## [0.3.2] - 2026-11-08

### Fixed
- **Systemd dependency issue**: Changed `Requires=libvirtd.service` to `Wants=libvirtd.service virtqemud.service`
  - Resolves "Dependency failed" error on systems with modular libvirt daemons
  - Now works with both monolithic libvirtd and modular virtqemud
  - Compatible with Debian 12+, Ubuntu 22.04+ (default modular setup)
  - Better error messages if libvirt is missing entirely

### Changed
- Systemd service: Hard dependency → Soft preference
- Added support for virtqemud (modular QEMU daemon)
- Service now starts even if libvirt daemons not present (fails with clear error later)

## [0.3.1] - 2026-11-08

### Fixed
- **Bootstrap systemd failure**: Environment file now written to `/etc/xdebbie/xdebbie_env` instead of `/root/.xdebbie_env`
  - Resolves "Read-only file system" error when service runs with ProtectHome=true
  - System-wide environment file accessible to all users
  - Graceful error handling (warns instead of failing)
- **Documentation**: Updated all references from `~/.xdebbie_env` to `/etc/xdebbie/xdebbie_env`
- **Installation**: Service now starts successfully after `dpkg -i`

### Changed
- Environment file constant: `.xdebbie_env` → `xdebbie_env`
- Bootstrap now writes to system directory instead of home directory
- postinst script updated with correct environment file location

## [0.3.0] - 2026-11-08

### Added
- **Auto-Discovery**: Multicast peer discovery on 239.0.0.42:7442
  - Automatic peer detection via multicast UDP
  - ANNOUNCE messages every 5s with node capabilities
  - HEARTBEAT messages every 1s for liveness
  - Zero-configuration clustering
  - Enabled by default, can be disabled
- **Leader Election**: Bully algorithm with score-based ranking
  - Score components: uptime (0-1000) + capacity (0-1000) + load (0-1000) + incumbent bonus (+500) + UUID tiebreaker
  - Automatic failover in <15s
  - ELECTION_PROPOSE and ELECTION_VOTE messages
  - Majority voting for leader selection
  - Self-stabilizing algorithm
- **High Availability**:
  - Leader runs active scheduler
  - Followers maintain shadow state
  - Automatic leader election on failure
  - No single point of failure
  - Peer timeout and cleanup (15s)
- **Documentation**:
  - Discovery and election guide (DISCOVERY.md)
  - Network requirements and troubleshooting
  - Monitoring and debugging instructions

### Changed
- Package description updated to reflect discovery features
- Config file includes discovery settings (enable_discovery, multicast_addr)
- Bootstrap generates discovery-enabled config by default
- Binary size increased to ~9.1 MB (includes discovery code)

### Technical
- New modules: `discovery/` (engine, messages, multicast, peer) and `election/` (state, scoring)
- Dependencies: Added `uuid` with serde support, `num_cpus`
- Discovery engine runs as async tokio task
- Multicast socket with TTL=1 (local network only)

## [0.2.0] - 2026-11-08

### Added
- **Self-Bootstrapping**: Complete automatic setup on stock Debian 13
  - Auto-installs dependencies (libvirt, qemu, cloud-image-utils)
  - Auto-creates libvirt network (virbr-xdebbie on 192.168.100.0/24)
  - Auto-creates storage pool (/var/lib/libvirt/xdebbie)
  - Generates configuration file (/etc/xdebbie/xdebbied.toml)
  - Generates secure API token (256-bit)
  - Creates client environment file (/etc/xdebbie/xdebbie_env)
- **Debian Package**: Professional .deb with systemd integration
  - Systemd service with proper network/disk ordering
  - Auto-runs bootstrap on first install
  - Starts on boot, restarts on failure
  - Clean upgrade/removal paths
  - Maintainer scripts (postinst, prerm, postrm)
- **CLI Tool (xdctl)**: Included in Debian package
  - `xdctl host ls` - List hypervisor hosts
  - `xdctl vm ls` - List virtual machines
  - `xdctl vm create` - Create VMs
  - `xdctl vm delete` - Delete VMs
  - JSON output support (`--json`)
- **Documentation**:
  - Bootstrap guide (BOOTSTRAP.md)
  - Quick start guide (docs/quickstart.md)
  - Migration guide (docs/migration.md)
  - Debian package documentation (docs/debian-package.md)
  - Complete design documents (docs/design/)

### Changed
- **Renamed from qfodufk8s to XDebbie**
  - qfodufk8sd → xdebbied (daemon)
  - qfodufk8sctl → xdctl (CLI tool)
  - CAPQ → CAPX (Cluster API Provider)
  - All paths updated (/etc/xdebbie, /var/lib/xdebbie, etc.)
  - Environment variables (XDEBBIE_URL, XDEBBIE_TOKEN)

### Fixed
- Package name typo in description ("XDebbie8s" → "XDebbie")
- OpenSSL development dependencies documented
- CAPX module reference (qfodufk8s.rs → xdebbie.rs)

## [0.1.0] - 2026-11-02

### Added
- Initial release as qfodufk8s
- VM orchestration via libvirt/QEMU
- HTTP API for VM management (:7443)
- Placement policies (spread/binpack)
- State persistence
- Cloud-init support (NoCloud datasource)
- Cluster API Provider (CAPQ)
- Basic CLI tool (qfodufk8sctl)

[0.3.2]: https://github.com/yourusername/xdebbie/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yourusername/xdebbie/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yourusername/xdebbie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yourusername/xdebbie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/yourusername/xdebbie/releases/tag/v0.1.0
