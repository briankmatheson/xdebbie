# Changelog

All notable changes to XDebbie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.16.6] - 2026-11-13

### Fixed
- `virsh start` on a domain that is already running is a no-op instead of
  an error. A VM whose domain briefly looked missing (e.g. across a daemon
  upgrade) was pushed back to `Provisioning` and then stuck there forever on
  "Domain is already active", which also blocked every volume attach to it
  ("waiting for vm (Provisioning)").

## [0.16.5] - 2026-11-13

### Added
- Followers register their hypervisor with whoever leads
  (`registrar.rs`): every 5 s each `[[hosts]]` entry is `PUT` to the
  leader with `uri = qemu+tls://<libvirtd listen address>/system` and the
  new `Host.agent` (this daemon's API URL), again a minute later in case
  the leader's store went backwards. Until now only `bootstrap --join`
  registered, once, with the CA box — so after a restart handed leadership
  to the joiner, the CA box's own host was unknown to the leader and a VM
  pinned there sat in `Pending: no host satisfies the request: 1×pinned
  elsewhere`.
- Followers mirror the leader's store (`mirror.rs`): `/v1/hosts`,
  `/v1/vms` and `/v1/volumes` every 5 s into the local `state.json` (own
  `[[hosts]]` entries kept as configured). A new leader starts from a copy
  seconds old instead of whatever its store held when it last led, and
  every box holds the VM records its DHCP responder answers from.
- The leader drives a host whose `agent` is another box through that
  box's daemon (`agent.rs`, `POST /v1/agent/<op>`, served by every box
  for itself, cluster token or peer certificate): the thin pool a root
  disk is carved from, the NVMe connects, the seed image and the base
  image live where the VM runs, which `qemu+tls://` from the leader
  cannot reach. `attach_volume`/`detach_volume` on a remote host, which
  bailed with "not supported yet", now work through the agent. Hosts
  without an `agent` (added by hand) are driven over `uri` as before.
  Closes the "known gap: the store is per box" in docs/autodiscovery.md.

### Changed
- The scheduler's 0.16.3 rule (no `storage = abe` VM on a host remote
  from the leader) now applies only to hosts without an `agent`. A host
  registered by its own daemon makes the ABE call and runs
  `nvme connect` on its own box, so the port fence follows.
- `Config::http_client` sets a 10 s connect timeout, so a peer that
  down costs the reconciler, the registrar, the mirror or an agent call
  seconds, not the kernel's SYN retry budget.

## [0.16.4] - 2026-11-13

### Fixed
- Domains are defined with 16 spare pcie-root-ports. On q35 every hot-plugged
  PCI device needs a free root port and libvirt only adds one or two beyond
  the defined devices, so attaching a third abe volume to a VM (abe-csi)
  failed with "No more available PCI slots". Root ports cannot be
  hot-plugged, so VMs defined before this need `virsh define` with the extra
  controllers and a restart to get them.

## [0.16.3] - 2026-11-13

### Fixed
- The scheduler no longer places a `storage = abe` VM on a host whose
  libvirt URI is remote from the leader. The leader runs `nvme connect`
  on its own box and then `virsh start` over `qemu+tls://`, so the VM
  failed every pass with `Cannot access storage file
  /dev/disk/by-id/nvme-uuid.…`. Such hosts are rejected with `abe on a
  remote host (nvme connect runs on the leader)` until the follower
  daemon does the attach (docs/storage.md, "What is not done").

## [0.16.2] - 2026-11-13

### Added
- capx: `XDEBBIE_CA` (`--xdebbie-ca`) — the CA that signs xdebbied's API
  certificate, as a PEM file path or the PEM text itself (a Secret key).
  Needed since xdebbied's API went https: a capx still pointed at
  `http://…:7443` fails every call, and VMs of a deleted cluster are
  never torn down (their `XDebbie8sMachine`s sit on the finalizer with
  `error sending request for url (http://…)`). Without it an `https://`
  URL warns that only the system roots are trusted.

## [0.16.1] - 2026-11-13

### Changed
- `xdctl host ls`: the NAME column widens to the longest host name
  (FQDNs no longer push the row out from under the headers); the VCPU
  and MEM GiB headers sit over their `used/total` cells.
- The leader's `abe utilization failed` / inventory warnings log the
  whole error chain (`error=abe utilization on gram…: tls: …` rather
  than just the outer context).
- `tests/abe/set-host-abe.sh <host> <url>` rewrites a host record's `abe`
  URL on the leader (GET, edit, PUT with the `XDEBBIE_*` environment).

## [0.16.0] - 2026-11-13

For ABE 0.5.0 (`abe-block-exporter`), whose API is now HTTPS with mutual
TLS and whose nvmet ports are fenced to the box that asked for them. Spec:
`specs/abe-tls.md`; milestone log: `milestones/2026-11-13-abe-tls.md`.

### Added
- `[[hosts]].abe` takes `https://10.22.<box>.1:443`. The ABE client is
  built from `Config::http_client` (`abe::Client::with_http`): the cluster
  CA (`ca_file` plus `peer_ca_dir`) as roots and this box's
  `tls_cert`/`tls_key` as its client certificate, so an ABE pointed at
  `/etc/xdebbie/api-*.pem` admits exactly the enrolled xdebbieds. The
  scheme is used as written: an `http://` record still reaches a 0.4.0
  ABE.
- A refused TLS handshake (untrusted server certificate, no client
  certificate accepted) is reported as `tls: <reason>` (an `AbeHttpError`
  with status 0, e.g. `tls: invalid peer certificate: UnknownIssuer`) in
  the log and the inventory error, instead of looking like a timeout.
- `Host.abeCapacity.tls` (from `/version`'s `"tls"`, abe ≥ 0.5.0, serde
  default `false`) and a `TLS` column in `xdctl host ls` (`yes` / `no` /
  `-`).
- `tests/lint-abe-client.sh`, run by `make test` and `make lint`: the bare
  `abe::Client::new()` is `#[cfg(test)]` and must not appear outside test
  code.
- `tests/abe/run.sh` phase `tls` (T1): a daemon with
  `tls_cert`/`tls_key`/`ca_file` from an openssl-made CA still provisions
  on the plain-http fake; `fake-abe.py` answers `"tls": false`.

### Changed
- Docs (`docs/storage.md`, `docs/api.md`): https host records, the ABE
  cert variables, a TLS section and a Port fence section (ABE opens an
  nvmet port only to the box that asked for it; after moving a VM or
  restoring ABE the next reconcile re-asks via the existing endpoint
  refresh — no new call sites).
- Debian revision back to 1 for 0.16.0.

## [0.15.0] - 2026-11-12

### Added
- ABE 0.4.0 client. `GET /version` is probed once a minute per server;
  a 404 means a pre-0.4 ABE and the daemon logs `abe at <url> predates
  0.4.0: no thin root clones, no name idempotency` once and keeps the 0.14
  behaviour. Every create carries `?name=` (`vol-<name>` for standalone
  volumes, `vm-<uuid>-root`/`-data` for VM disks), so a retry after a crash
  gets the same volume back instead of a second one.
- Thin root clones. With `base` and `clone` features the root disk of a
  `storage = abe` VM is a thin clone of a per-image base volume
  (`?base=<image>`, filled once with `qemu-img convert`, flushed and
  `/seal`ed under a per-(server, image) lock). `Vm.disks[].origin` records
  the base. Provisioning takes seconds instead of a full image copy;
  `disk_gib` smaller than the image's virtual size is refused.
- ABE capacity accounting. The inventory polls `GET /utilization` into
  `Host.abeCapacity` (kept on error, treated as unknown when older than
  `host_stale_secs`). ABE VMs are placed by `abe_fit`: `no abe`, `abe
  capacity unknown`, `abe pool full` (`abe_max_data_percent` /
  `abe_max_metadata_percent`, defaults 85/80) or `abe capacity`
  (`provisioned + pending + request > bytes × abe_overcommit`, default 1.0)
  keep the VM `Pending` with the reasons in `message`; the libvirt pool is
  no longer consulted for them. Pending disks and in-flight `POST
  /v1/volumes` count. `xdctl host ls` shows an `ABE GiB` column.
- Endpoint refresh. Before a cold start, before provisioning a VM whose
  disks exist and before attaching a standalone volume, the daemon asks
  `GET /id/<id>` where each unconnected disk is exported now; if ABE moved
  it (restart, lowest-free port) the stale controller is dropped, the
  record updated and `abe moved <id> from <old> to <new>; record updated`
  logged.
- `POST /v1/volumes` picks the ABE host with the most headroom (ties by
  name) and answers **507** when none fits; creates are serialized per
  name, so five concurrent POSTs of one name make one ABE call (one 201,
  four 200). `Volume.deleting`: DELETE marks the record first, answers
  **502** if ABE fails and leaves the flag (attach → 409 `volume is being
  deleted`, the reconciler never attaches it); a second DELETE retries.
- `tests/abe/run.sh` runs five phases against a fake ABE 0.4.0 (thin,
  legacy, crash after `/seal`, capacity, no ABE host); `tests/lint-links.sh`
  fails on the k-less spelling of the GitHub account in repo links.
- `bootstrap --join` copies the overlay `[network]` from the CA box
  (`GET /v1/network` over mTLS): same `cidr`/`domain`, and this box's own
  /24 of it. Added to a kept config that lacks `[network]`, in place of
  the template in a regenerated one. Warns if the CA box's pool overlaps
  the /24.
- Overlay addressing is one rule: a /16 (default `10.23.0.0/16`, the new
  `--subnet` default), and box *N* owns `10.23.N.0/24` — bridge at `.N.1`
  (its VMs' gateway and DNS, and where the mesh's derived probe already
  looked), VMs at `.N.2`–`.254`. Replaces the first-host gateway shared
  across boxes, the `cidr + box·256` pool slices with their 16-address
  carve-out, and the `reserved` lists of other boxes' bridges. Bootstrap
  creates the libvirt network at the box's address and warns, with the
  `virsh net-edit` to run, when an adopted bridge sits elsewhere.

- libvirt TLS comes from the cluster CA. Step 7 installs `api-ca.pem` as
  libvirt's CA and this box's enrolled certificate as both server and
  client cert, so every box's libvirtd accepts every other box's client
  with nothing copied by hand. The per-box RSA "XDebbie CA" and its
  `openssl` dependency are gone. libvirtd listens on the overlay bridge
  address; bootstrap warns when the certificate lacks a SAN for it.
- `bootstrap --join` registers the box with the cluster (step 9):
  `PUT /v1/hosts/<fqdn>` at the CA box with the local `[[hosts]]` entry and
  `uri = qemu+tls://<overlay address>/system`. No more `xdctl host add`
  for a joined box.

- VMs can leave the box. The libvirt network is `<forward mode='route'/>`
  (isolated networks make libvirt reject all forwarding), `ip_forward` is
  on, ufw forwards the box's /24 out of the bridge, and a new `ip
  xdebbie_host` nftables table masquerades that /24 behind whatever
  interface the packet leaves by — each box NATs its own VMs and nobody
  else's, VM-to-VM stays on the mesh L2. VM DNS (the bridge, port 53
  udp/tcp) is DNAT'd to `[network] upstream_dns`: the first box's own
  resolver, copied to every joiner via `/v1/network`, so every VM asks the
  same server. New flows into the bridge from any other interface are
  dropped. Bootstrap warns on an adopted network without `<forward>`.
- The LAN interface accepts only ssh, WireGuard, the API and discovery
  multicast. libvirt TLS is accepted on the underlay and bridge only
  (libvirtd listens on the bridge address anyway), Geneve on the
  underlay, DHCP on the bridge. ufw rules are tagged with what they are
  for and tagged rules no longer wanted are deleted, so existing boxes
  converge on the next start.

### Fixed
- API certificates now carry `127.0.0.1` and `::1` SANs, so `xdctl` with
  its default `--url https://127.0.0.1:7443` verifies on the box itself.
- `xdctl mesh …` always followed the leader, so from a follower it silently
  edited the *leader's* peer list and the follower's own mesh could not be
  enrolled with `xdctl` at all. `mesh` is per-box state: it now stays on
  the box at `--url`/`XDEBBIE_URL`. Everything else still goes to the leader.
- Bootstrap built `advertise_url`, the join `api_url` and the registered
  libvirt URI from `hostname -f` even when that name resolves nowhere (or
  only to IPv6 link-local / container addresses), leaving URLs no other box
  could open. Bootstrap now checks the lookup first (2 s budget) and, when
  it fails, uses the address this box announces on discovery throughout;
  a kept `advertise_url` built from the dead name is regenerated. The
  hostname remains the box's name (enrolment, `xdctl host ls`, cert CN).
- `docs/storage.md` and this changelog linked ABE at a misspelt GitHub
  account (the k was missing); fixed and guarded by `tests/lint-links.sh`.
- Bootstrap wrote the bridge of whichever libvirt network sorted first
  (`default`/`virbr0`, usually inactive) into `[mesh]` and `[[hosts]]`
  instead of the `xdebbie` network it had just ensured. The mesh then sat on
  `BridgeMissing virbr0` and VMs would have landed on a bridge that does not
  exist. Now the network and pool bootstrap ensured are selected by name.
- Bootstrap commented `[network]` out for any adopted libvirt network,
  assuming dnsmasq. It now reads the network XML: no `<dhcp>` range means
  the bridge is xdebbied's to serve and `[network]` is written live with the
  bridge's actual subnet.

## [0.14.2] - 2026-11-12

### Added
- Discovery `ANNOUNCE` carries the sender's addresses (`addrs`). When a
  peer's `api_url` hostname does not resolve locally, the daemon dials it by
  the datagram's source address (or an announced one) instead; API
  certificates already list every local address as a SAN. `xdctl leader`
  shows the address form when this happens. The bootstrap discovery listen
  does the same for the join URL it offers. Pre-0.14.2 daemons interoperate.
- Discovery joins the multicast group on every interface with a global IPv4
  address, not only the default route's. A box whose default route leaves by
  a bridge or VPN no longer goes deaf to peers on the LAN.

### Changed
- Election score gains a state term: 2000 for any VM in the box's store plus
  200 per VM (capped at ten), outweighing uptime, capacity and the incumbent
  bonus together. Restarting the leader no longer hands the cluster to a
  peer with an empty store; it takes leadership back within seconds.

## [0.14.1] - 2026-11-12

### Added
- **Box ids are assigned by the cluster.** `--box` is no longer needed:
  the first box is 0 and the CA box hands each joiner the lowest free id at
  enrolment (own id + configured peers + every live enrolment are taken; a
  host that re-enrols keeps its id; `--box` is a wish honoured if free).
  Bootstrap now enrols *before* creating the libvirt network so the
  assigned id drives the bridge MAC and `[mesh] box`. `POST /v1/enroll`'s
  `box` is optional; the reply carries the assigned one.

### Fixed
- A kept `xdebbied.toml` (no `--force`) now learns what the cluster
  decided: its `[mesh] box` line and, for a joiner, `ca_url` are rewritten
  in place with comments intact. Previously a joiner with an existing
  config ended up without `ca_url`, so `xdctl approve` from it could not
  be forwarded and a later `bootstrap` did not know its cluster.
- `bootstrap` bails up front unless run as root, instead of getting as far
  as minting a new key and failing on `/etc/xdebbie`.
- `bootstrap --join` without `--force` never replaces an existing
  `api-key.pem` when there is no enrolment to resume (that would orphan
  an approved certificate); it says to pass `--force`.

## [0.14.0] - 2026-11-12

### Added
- **Cluster enrolment with operator approval.** One CA per cluster: the
  first box bootstrapped keeps the CA key (`/etc/xdebbie/api-ca-key.pem`,
  root:root 0400). A new box runs
  `xdebbied bootstrap --box N --join https://first:7443 [--ca-fingerprint …]`:
  it pins the CA (y/N prompt or pinned fingerprint), generates a key and
  CSR, `POST /v1/enroll`s it and waits. An operator runs `xdctl enroll ls`
  and `xdctl approve <host>` (or `xdctl enroll deny`) from any member; the
  CA box signs, the joiner installs the cert and fetches the cluster token
  over mTLS (`GET /v1/enroll/token`). No tokens or CA files to copy by hand.
- **Client-certificate authentication.** The API asks every TLS client for
  a certificate; one signed by the cluster CA is accepted in place of a
  bearer token. Daemons present their own cert to each other, so leader
  forwarding and token distribution work before tokens are in sync (the
  distributor now also replaces a peer's token when the peer answered on
  its certificate but holds a different one).
- API: `GET/POST /v1/enroll`, `GET /v1/enroll/{id}`,
  `POST /v1/enroll/{id}/{approve,deny}` (forwarded to the CA box via the
  new `ca_url` config key), `GET /v1/enroll/token` (client cert only),
  `GET /v1/ca` (public; `X-Xdebbie-CA-Fingerprint` header).
- `xdebbied bootstrap` with no `--join` listens 20 s for a running
  cluster's discovery announcements and offers to join it (leader first);
  hearing nothing, it asks for a URL, Enter = first box. Non-interactive
  (package install): a found cluster is an error pointing at `--join`.
  `--standalone` and `--discover-secs N` (0 = off) skip the wait.
- xdctl: `enroll ls [--all]`, `enroll approve|deny`, `approve <host>`, `ca`.
- Config: `ca_key` (default `/etc/xdebbie/api-ca-key.pem`), `ca_url`
  (set by `--join`, kept across `--force`; a new `--join` replaces it).

### Changed
- `bootstrap --force` on the CA box reissues the server cert but keeps the
  CA. Certs are issued with serverAuth + clientAuth.
- Bootstrap runs before the async runtime starts (plain blocking work).
- The peer CA directory (`/etc/xdebbie/peers`) is still honoured but no
  longer needed: every box's cert chains to the cluster CA.

## [0.13.0] - 2026-11-12

### Security
- **The API serves HTTPS out of the box.** Bootstrap generates a per-box CA
  and a server certificate signed by it (in-process with `rcgen`, no
  openssl) as `/etc/xdebbie/api-{ca,cert,key}.pem`, and writes
  `tls_cert`/`tls_key` into the config. The bearer token and the WireGuard
  public keys exchanged during mesh enrolment no longer cross the LAN in
  clear. The server cert's SANs cover the hostname, FQDN and every local
  IPv4 address; `bootstrap --force` reissues it after an address change.
- Trust between boxes: each daemon and `xdctl` trust the box's own CA
  (`ca_file`) plus every `*.pem` in `peer_ca_dir` (`/etc/xdebbie/peers`).
  Copy `api-ca.pem` from each box into the other's `peers/` before
  enrolling (recipe in BOOTSTRAP.md). `xdctl --ca` / `XDEBBIE_CA` take a
  comma-separated list of files or directories; `--insecure` /
  `XDEBBIE_INSECURE` skips verification for a lab.
- `xdebbie_env` and `xdebbied.toml` are root:xdebbie 0440 and the config
  directory root:xdebbie 0750. Bootstrap and the package postinst create
  the `xdebbie` system group; add an operator to it
  (`usermod -aG xdebbie`) and `source /etc/xdebbie/xdebbie_env` works
  without sudo. `token`, `token.previous` and `api-key.pem` stay root:root
  0400. Token rotation keeps the env file's group mode.

### Fixed
- Bootstrap wrote `advertise_url` commented out and `XDEBBIE_URL` as
  `http://0.0.0.0:7443`, so peers announced `127.0.0.1` in discovery and
  `xdctl leader` showed every box on loopback. `advertise_url` is now
  emitted live as `https://<fqdn>:7443` and the env file carries the same
  URL (plus `XDEBBIE_CA`).
- Package postinst forced `xdebbie_env` back to root:root 0400 on every
  upgrade; it now applies the 0440/0400 split above.

### Changed
- `xdctl` default `--url` is `https://127.0.0.1:7443`.
- New config keys `ca_file` (default `/etc/xdebbie/api-ca.pem`) and
  `peer_ca_dir` (default `/etc/xdebbie/peers`).
- `examples/four-node/prep-host.sh` masks display managers, cups,
  bluetooth, upower and power-profiles-daemon, sets `multi-user.target`
  and tells logind to ignore lid/idle/suspend — a repurposed desktop
  otherwise goes to sleep under the cluster.

## [0.12.0] - 2026-11-12

### Security
- `/etc/xdebbie/xdebbie_env` was written 0644 by bootstrap and by token
  rotation — the API token, world-readable. It, `token` and
  `token.previous` are now root:root 0400, written to a temp file that
  already has that mode and renamed into place. The package postinst
  fixes existing files on upgrade. Source the env file from a root shell.

### Changed
- xdctl verbs: every `ls` (`host`, `vm`, `volume`, `mesh`, `net`) also
  answers to `list`, `show` and `get`; `net show` is now `net ls`. The
  long form of one object is `describe`: `vm describe <name|id>`,
  `volume describe <id>`, `host describe <name>` (new), `mesh describe`
  (the full `{spec, status}` JSON, formerly `mesh get`). `vm get <id>` and
  `volume get <id>` no longer exist — `get` lists now.
- **The mesh is on by default.** A config with no `[mesh]` section now
  meshes on the first host's bridge with a managed WireGuard underlay
  (`wg-flock`, 10.22.`<box>`.1/16, port 51820) and zero peers: the bridge
  MTU is pinned, the `xdebbie_mesh` nftables table installed and the
  WireGuard link keyed, so the box is ready to be enrolled from another one
  without editing anything. Inside `[mesh]`, `enabled` defaults to true,
  `bridge` defaults to the first `[[hosts]]` bridge, and `[mesh.wireguard]`
  is assumed unless `allow_plain_underlay = true` (you bring the underlay).
  `enabled = false` opts out. `PUT /v1/mesh` without `enabled` also means
  enabled now. Bootstrap writes the `[mesh]` section out live instead of
  commented, and its ufw rules follow the effective mesh (WireGuard and
  Geneve ports open on a default config).
- Why: the regeneration bug fixed in 0.11.2 showed that a dropped `[mesh]`
  silently took a box out of the mesh. The default should be the thing the
  rest of the system assumes.
- `xdebbied bootstrap --box <0-253>` (default 0) sets the mesh box id in
  the generated `[mesh]` and gives the libvirt network bootstrap creates the
  bridge MAC `52:54:00:23:<box>:fe` that the derived FDB scheme expects —
  previously libvirt picked a random one and the mesh reported
  `BridgeMacMismatch` forever. An existing network is not changed; bootstrap
  warns and says how to fix it.

## [0.11.2] - 2026-11-12

### Fixed
- Bootstrap replaced a hand-edited `xdebbied.toml` whenever it failed to
  parse (a typo was enough), silently dropping `volume_group`, `[network]`,
  `[mesh]` and the rest. It now leaves such a file alone and warns; with
  `--force` it backs the old file up as `.bak-<timestamp>` and carries the
  operator's settings (`volume_group`, `thin_pool`, `tls_*`, `advertise_url`,
  `policy`, `reconcile_interval_secs`, `host_stale_secs`, `[network]`,
  `[mesh]`, `[[hosts]]`) into the regenerated one. A `--force` token
  regeneration is backed up and warned about too.
- A config, state or token file that does not load now exits with status
  78 (`EX_CONFIG`) and `xdebbied.service` lists it in
  `RestartPreventExitStatus`, so the failure stays visible in
  `systemctl status` instead of cycling through bootstrap every few
  seconds.

## [0.11.1] - 2026-11-12

### Fixed
- xdebbied exited at startup (`missing field iodepth`) on a state file
  holding a volume created by 0.9.1 or older, whose `perf` had the earlier
  shape. `perf` is an annotation: one that does not parse is now read as
  `null` instead of making the whole state file unreadable. `Store::open`
  errors name the file.

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
  hypervisor runs an [ABE](https://github.com/briankmatheson/abe) NVMe/TCP
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
