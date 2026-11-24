# REST API (v1)

Every route the xdebbied API serves, who may call it, and what it returns.

The base URL is `https://BOX:23443`. With `token_file` configured, every
`/v1` call needs `Authorization: Bearer TOKEN` **or** a TLS client
certificate signed by the cluster CA (that is how daemons talk to each
other). Routes marked *public* need neither. Bodies are JSON with camelCase
keys, except where noted. Errors are `{"error": "..."}`.


Any box in a cluster answers. Hosts, VMs, volumes, failure domains and
`/v1/schedule` are decided by the elected leader; a follower forwards them
there and relays the reply (503 with `Retry-After` during an election, 502
if the leader is unreachable). `/v1/leader`, `/v1/network`, `/v1/mesh*` and
`/v1/token`, `/v1/version`, `/v1/settings` and `/v1/client-config` describe
the box you asked. See [Discovery, election and the leader](../autodiscovery.md).

## Version

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/version` | *Public, box-local.* `{version, hostname, features}` |

## Leader

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/leader` | `{this, state, is_leader, leader, peers}`; `leader` is `null` during an election. `xdctl` calls this first and talks to `leader.api_url` |

## Token

One bearer token per cluster; rotation overlaps the old and new until every
node accepts the new one ([Rotate the API token](../how-to/rotate-token.md)). Fingerprints are the first
12 hex of SHA-256; the token itself is shown only by `rotate`.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/token` | Local to the box asked: `{id, previous, currentUsed, rotating, nodes}`. `nodes` (leader only) lists each peer as `current`, `overlap`, `installing`, `stale` or `unreachable: …` |
| PUT | `/v1/token` | Local. `{current, previous}` — the set this box must accept. What the leader pushes to peers; authenticate with any token the box still holds |
| POST | `/v1/token/rotate` | Leader. 201 `{token, id, previous}`; the only time a token is returned. 409 while an earlier rotation is still being distributed |

## Settings

This box's runtime switches, applied in place (no restart) and kept in
`settings.json` beside `state_file`, which overrides `xdebbied.toml`.

```text
GET /v1/settings   -> {"mesh_routed": false, "discovery": false}
PUT /v1/settings   {"discovery": true}   -> the whole object, as now applied
```

| key | effect | default |
|---|---|---|
| `mesh_routed` | routed mesh peers ([xdj](https://github.com/briankmatheson/xdj)) may open connections to this box's VMs: one tagged nft accept in `xdebbie_host` and one ufw route rule, added or removed alone | off |
| `discovery` | LAN discovery and election; off pauses the engine (this box leads alone) and closes the discovery port in ufw | off |

A `PUT` that cannot be applied answers `409` with the reason (for
example `mesh_routed` on a box without `[network]`). Besides the cluster
token and cluster client certificates, the token in
`settings_token_file` is accepted here, and answers `403` everywhere
else: hand that one to a caller that only flips these switches, such as a
dashboard.

Likewise the token in `schedule_token_file` opens only the dry run,
`POST /v1/schedule`, and answers `403` everywhere else (reads included).
It creates nothing: hand it to a caller that asks whether one more VM
would fit before it asks a cluster for one.

## Enrolment

How a box joins ([Bootstrap](../bootstrap.md#joining-a-cluster-enrolment-and-approval)). Decided by the box
holding the CA key; any other member forwards these to it (`ca_url`), 409
if it cannot. `xdebbied bootstrap --join` is the only client of the
public ones for boxes; xdj is the client for `kind: "client"`.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/ca` | *Public.* Cluster CA certificate, PEM; `X-Xdebbie-CA-Fingerprint: sha256:…` |
| GET | `/v1/cert` | *Public, box-local.* The chain this box serves, leaf first: `[{subject, issuer, serial, not_before, not_after, fingerprint, self_signed, pem}]` (`xdctl ca --chain`) |
| POST | `/v1/enroll` | *Public.* `{kind?, hostname, box?, api_url, csr, wg_pubkey?}`. `kind` is `"client"` for an xdj client (its pending request is denied after 15 minutes); omitted for a box → 202 `{id, hostname, box, api_url, fingerprint, status: "pending", requested_at, from?}`. A box's CSR must name `hostname` (its CN). `box` in the request is a wish (400 if taken); the reply carries the id the CA assigned — lowest free, or the one this hostname was approved with before. Same key again returns the same enrolment; at most 64 pending |
| GET | `/v1/enroll/{id}` | *Public.* The enrolment; once `approved` it carries `cert` and `ca` (PEM) |
| GET | `/v1/enroll?all=true` | Pending enrolments (`all`: approved and denied too). The `enroll_token_file` token sees client requests only |
| POST | `/v1/enroll/{id-or-host}/approve` | Sign the CSR with the cluster CA |
| POST | `/v1/enroll/{id-or-host}/deny` | Refuse a pending request. An approved box cannot be denied afterwards: its certificate stays valid (there is no revocation) and so does its hold on its box id |
| POST | `/v1/enroll/renew` | `{csr}` → `{cert, ca}`. An xdj client renews its certificate with the one it holds; no approval. Client certificates only |
| GET | `/v1/enroll/token` | Local. `{current, previous}` for a caller with a **client certificate** (403 for a bearer token) |

## Hosts

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/hosts` | List hosts, with discovered `capacity`, `lastSeen` and, for hosts with an `abe`, the last `/utilization` reading as `abeCapacity` (`thin`, `bytes`, `usedBytes`, `freeBytes`, `provisionedBytes`, `dataPercent`, `metadataPercent`, `tls` — the ABE's API is behind mutual TLS, abe ≥ 0.5.0 — `seen`) |
| GET | `/v1/hosts/{name}` | |
| PUT | `/v1/hosts/{name}` | Upsert. 201 created, 200 updated. Keeps runtime fields. `agent` (optional): API URL of the xdebbied on that host; the leader sends host-local work there (set by that daemon's registrar, see [Discovery](../autodiscovery.md#the-store-is-per-box-followers-keep-up)) |
| DELETE | `/v1/hosts/{name}` | 409 if VMs remain — drain first |
| POST | `/v1/hosts/{name}/cordon` | No new placements |
| POST | `/v1/hosts/{name}/uncordon` | |
| POST | `/v1/hosts/{name}/drain` | Cordon and plan live migrations. Returns `{moves, stuck}` |
| GET | `/v1/failure-domains` | `[{name, controlPlane, hosts}]` |

Host body:

```json
{"name": "box1", "uri": "qemu+tls://10.23.1.1/system", "failureDomain": "fd-1",
 "controlPlaneEligible": true, "labels": {"disk": "nvme"}, "storagePool": "default",
 "bridge": "virbr-xdebbie", "cpuOvercommit": 4.0, "memOvercommit": 1.0,
 "reservedMemoryMib": 4096, "reservedCpus": 2,
 "abe": "https://10.22.1.1:443", "abeOvercommit": 1.0,
 "abeMaxDataPercent": 85.0, "abeMaxMetadataPercent": 80.0}
```

## VMs

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/vms` | All VMs. `?selector=KEY=VALUE,KEY2=VALUE2` filters by labels |
| PUT | `/v1/vms/{uuid}` | Idempotent create. 201 new, 200 same spec, 409 different spec or duplicate name |
| GET | `/v1/vms/{uuid}` | |
| DELETE | `/v1/vms/{uuid}` | 202; poll GET until 404. A VM labelled `xdebbie.io/protected=true` is refused unless `?force=true` |
| POST | `/v1/vms/{uuid}/start` | Desired power on |
| POST | `/v1/vms/{uuid}/stop` | Desired power off (ACPI shutdown) |
| POST | `/v1/vms/{uuid}/reboot` | Running VMs only |
| POST | `/v1/vms/{uuid}/migrate` | `{"host": "box2"}`, or `{}` for the scheduler's choice |
| POST | `/v1/schedule` | Dry run. Body is a VmSpec; returns ranked hosts or 422 with reasons |
| POST | `/v1/agent/{op}` | Internal. This box, never forwarded: the leader's hypervisor calls for hosts this daemon owns (`inventory`, `abe-utilization`, `refresh-endpoints`, `state`, `prepare-disks`, `provision`, `start`, `shutdown`, `reboot`, `destroy`, `addresses`, `tap-owners`, `migrate`, `attach-volume`, `detach-volume`). Body `{host, vm, ...}`, reply the call's result; cluster token or peer certificate |

VmSpec:

```json
{"name": "demo-md-0-abcde", "vcpus": 4, "memoryMib": 12288, "diskGib": 80,
 "image": "debian-13-k8s-v1.36",
 "placement": {"failureDomain": "fd-2", "antiAffinityGroup": "default.demo.workers",
               "strictAntiAffinity": false, "requiredLabels": {}, "controlPlane": false, "host": null},
 "userData": "#cloud-config\n...", "metaData": {"cluster": "demo"},
 "labels": {"xdebbie.dev/cluster": "default.demo"}}
```

VMs are immutable. To resize, create a new VM and delete the old one, which
is what Cluster API does anyway.

Vm status fields: `phase` (`Pending`, `Provisioning`, `Running`, `Stopped`,
`Migrating`, `Deleting`, `Failed`), `host`, `addresses[{type, address}]`,
`message` (last error or wait reason).

## Volumes

Standalone ABE volumes with their own lifecycle, hot-plugged into VMs as
virtio disks. The host does the NVMe/TCP; the guest never needs a route to
ABE. This is what the `abe-csi` driver uses from inside Kubernetes VMs.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/volumes` | |
| POST | `/v1/volumes` | `{"name": "pvc-…", "bytes": N, "host": null}`. Synchronous: asks ABE (`?name=vol-NAME`, so a retry gets the same volume) and returns 201 with the id ABE assigned (200 if a volume with that name already exists, 409 if it is smaller than asked). Concurrent POSTs of one name are serialized: one ABE call, one 201, the rest 200. Without `host` the ABE with the most headroom is used. 502 when ABE fails, 503 with no healthy ABE host, **507** when no ABE host fits (`no ABE host fits BYTES bytes: HOST: REASON, …`; reasons are `abe capacity`, `abe pool full`, `abe capacity unknown`, `no abe`, `unhealthy`) |
| GET | `/v1/volumes/{id}` | |
| DELETE | `/v1/volumes/{id}` | 204; 409 while attached (detach first). Synchronous on ABE: the record is marked `deleting: true` first, then ABE is called. **502** if ABE fails; the record stays with `deleting: true` (the reconciler will not attach it) and a second DELETE retries |
| POST | `/v1/volumes/{id}/attach` | `{"vm": "VM_UUID", "force": false}` → 202. The reconciler connects on the VM's host and hot-plugs; poll until `attached.vm` matches. 409 if a *running* VM still has it, unless `force`; **409** `volume is being deleted` for a volume with `deleting: true` |
| POST | `/v1/volumes/{id}/detach` | 202; poll until `attached` is absent |
| POST | `/v1/volumes/{id}/grow` | `{"bytes": N}` → 200 with the volume. Grows on its ABE (abe >= 0.7.2, feature `grow`; rounded up to a MiB), never shrinks: no bigger than it is answers 200 unchanged. Where it is attached, the VM's host passes the new size to the guest (`virsh blockresize`) within a tick or two; the filesystem is the guest's to grow. **501** when the ABE cannot grow, **409** when ABE's kernel cannot re-read the size live, **507** when ABE has no room |

Volume:

```json
{"id": "7c0a…(32 hex)", "name": "pvc-1b2c", "server": "https://10.22.0.1:443",
 "traddr": "10.22.0.1", "port": "44002", "bytes": 5368709120,
 "perf": {"bs": "4k", "iodepth": 1, "seconds": 5, "write_lat_us": 187.5, "write_iops": 5000.0, "write_mib_s": 19.5},
 "attachTo": "VM_UUID", "attached": {"vm": "VM_UUID", "host": "box0", "target": "sdb"},
 "message": null, "deleting": false}
```

`attachTo` is the wish, `attached` what the reconciler did. Volumes go on
the VM's virtio-scsi controller (`target` `sdb`, `sdc`, ...). Inside the
guest the disk is `/dev/disk/by-id/scsi-0QEMU_QEMU_HARDDISK_ID` (the whole
id). A VM with no PCI slot left for the controller gets a virtio-blk disk
instead (`target` `vdc`, ...), at `/dev/disk/by-id/virtio-` plus the first
20 characters of the id. A VM that is deleted while holding a volume just
drops it: the volume survives, unattached. `perf` is ABE's fio
characterization when it ran one.

## QoS

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/qos` | Every host's ABE IOPS arbiter report (ABE 0.7.0 and later), merged: `{"volumes": {"ID": {...}}, "hosts": {...}}`. Cached 2 s. An ABE that is down, older or has its arbiter off adds nothing |

## Mesh

Box-local, never forwarded. Details in [Geneve mesh over WireGuard](../mesh.md#api).

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/mesh` | `{spec, status}`. An xdj client certificate may read it |
| PUT | `/v1/mesh` | Replace the spec. `If-Match: GENERATION` → 409 on mismatch; 400 with `errors[]` |
| DELETE | `/v1/mesh` | Forget the API spec; `[mesh]` from the config applies again |
| GET | `/v1/mesh/peers/{box}` | One peer's status |
| POST | `/v1/mesh/reconcile` | 202; reconcile now |

## Network

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/network` | `[network]` as applied, DHCP responder state, counters and DNS reservations. 404 without `[network]` |

## Client configuration (xdj)

Box-local, never forwarded. What this box tells its xdj clients.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/client-config` | *Public.* `{dns: [{zone, server}], search, welcome}` |
| GET | `/v1/client-config/xdj` | *Public.* The same, as text for xdj |
| PUT | `/v1/client-config` | The whole document. Cluster token or cluster certificate. Kept in `client-config.json` beside `state_file` |

