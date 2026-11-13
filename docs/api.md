# xdebbied REST API (v1)

Base URL `https://<xdebbied>:7443`. With `token_file` configured, every `/v1`
call needs `Authorization: Bearer <token>` **or** a TLS client certificate
signed by the cluster CA (that is how daemons talk to each other). The
enrolment routes marked *public* below need neither. Bodies are JSON with
camelCase keys. Errors are `{"error": "..."}`.

Any box in a cluster answers. Hosts, VMs, volumes, failure domains and
`/v1/schedule` are decided by the elected leader; a follower forwards them
there and relays the reply (503 with `Retry-After` during an election, 502
if the leader is unreachable). `/v1/leader`, `/v1/network`, `/v1/mesh*` and
`/v1/token` and `/healthz` describe the box you asked. See
[autodiscovery.md](autodiscovery.md).

## Leader

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/leader` | `{this, state, is_leader, leader, peers}`; `leader` is `null` during an election. `xdctl` calls this first and talks to `leader.api_url` |

## Token

One bearer token per cluster; rotation overlaps the old and new until every
node accepts the new one (docs/operations.md). Fingerprints are the first
12 hex of SHA-256; the token itself is shown only by `rotate`.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/token` | Local to the box asked: `{id, previous, currentUsed, rotating, nodes}`. `nodes` (leader only) lists each peer as `current`, `overlap`, `installing`, `stale` or `unreachable: …` |
| PUT | `/v1/token` | Local. `{current, previous}` — the set this box must accept. What the leader pushes to peers; authenticate with any token the box still holds |
| POST | `/v1/token/rotate` | Leader. 201 `{token, id, previous}`; the only time a token is returned. 409 while an earlier rotation is still being distributed |

## Enrolment

How a box joins (BOOTSTRAP.md, "Joining a cluster"). Decided by the box
holding the CA key; any other member forwards these to it (`ca_url`), 409
if it cannot. `xdebbied bootstrap --join` is the only client of the
public ones.

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/ca` | *Public.* Cluster CA certificate, PEM; `X-Xdebbie-CA-Fingerprint: sha256:…` |
| POST | `/v1/enroll` | *Public.* `{hostname, box?, apiUrl, csr, wgPubkey?}` → 202 `{id, hostname, box, apiUrl, fingerprint, status: "pending", requestedAt}`. `box` in the request is a wish (409-style error if taken); the reply carries the id the CA assigned — lowest free, or the one this hostname was approved with before. Same key again returns the same enrolment; at most 64 pending |
| GET | `/v1/enroll/{id}` | *Public.* The enrolment; once `approved` it carries `cert` and `ca` (PEM) |
| GET | `/v1/enroll?all=true` | Pending enrolments (`all`: approved and denied too) |
| POST | `/v1/enroll/{id-or-host}/approve` | Sign the CSR with the cluster CA |
| POST | `/v1/enroll/{id-or-host}/deny` | Refuse |
| GET | `/v1/enroll/token` | Local. `{current, previous}` for a caller with a **client certificate** (403 for a bearer token) |

## Hosts

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/hosts` | List hosts, with discovered `capacity`, `lastSeen` and, for hosts with an `abe`, the last `/utilization` reading as `abeCapacity` (`thin`, `bytes`, `usedBytes`, `freeBytes`, `provisionedBytes`, `dataPercent`, `metadataPercent`, `tls` — the ABE's API is behind mutual TLS, abe ≥ 0.5.0 — `seen`) |
| GET | `/v1/hosts/{name}` | |
| PUT | `/v1/hosts/{name}` | Upsert. 201 created, 200 updated. Keeps runtime fields |
| DELETE | `/v1/hosts/{name}` | 409 if VMs remain — drain first |
| POST | `/v1/hosts/{name}/cordon` | No new placements |
| POST | `/v1/hosts/{name}/uncordon` | |
| POST | `/v1/hosts/{name}/drain` | Cordon and plan live migrations. Returns `{moves, stuck}` |
| GET | `/v1/failure-domains` | `[{name, controlPlane, hosts}]` |

Host body:

```json
{"name": "srv1", "uri": "qemu+ssh://xdebbie@srv1/system", "failureDomain": "fd-1",
 "controlPlaneEligible": true, "labels": {"disk": "nvme"}, "storagePool": "default",
 "bridge": "br0", "cpuOvercommit": 4.0, "memOvercommit": 1.0,
 "reservedMemoryMib": 4096, "reservedCpus": 2,
 "abe": "https://10.22.1.1:443", "abeOvercommit": 1.0,
 "abeMaxDataPercent": 85.0, "abeMaxMetadataPercent": 80.0}
```

## VMs

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/vms?selector=k=v,k2=v2` | Filter by labels |
| PUT | `/v1/vms/{uuid}` | Idempotent create. 201 new, 200 same spec, 409 different spec or duplicate name |
| GET | `/v1/vms/{uuid}` | |
| DELETE | `/v1/vms/{uuid}` | 202; poll GET until 404 |
| POST | `/v1/vms/{uuid}/start` | Desired power on |
| POST | `/v1/vms/{uuid}/stop` | Desired power off (ACPI shutdown) |
| POST | `/v1/vms/{uuid}/reboot` | Running VMs only |
| POST | `/v1/vms/{uuid}/migrate` | `{"host": "srv2"}` or `{}` for scheduler's choice |
| POST | `/v1/schedule` | Dry run. Body is a VmSpec; returns ranked hosts or 422 with reasons |

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
| POST | `/v1/volumes` | `{"name": "pvc-…", "bytes": N, "host": null}`. Synchronous: asks ABE (`?name=vol-<name>`, so a retry gets the same volume) and returns 201 with the id ABE assigned (200 if a volume with that name already exists, 409 if it is smaller than asked). Concurrent POSTs of one name are serialized: one ABE call, one 201, the rest 200. Without `host` the ABE with the most headroom is used. 502 when ABE fails, 503 with no healthy ABE host, **507** when no ABE host fits (`no ABE host fits <bytes> bytes: <host>: <reason>, …`; reasons are `abe capacity`, `abe pool full`, `abe capacity unknown`, `no abe`, `unhealthy`) |
| GET | `/v1/volumes/{id}` | |
| DELETE | `/v1/volumes/{id}` | 204; 409 while attached (detach first). Synchronous on ABE: the record is marked `deleting: true` first, then ABE is called. **502** if ABE fails; the record stays with `deleting: true` (the reconciler will not attach it) and a second DELETE retries |
| POST | `/v1/volumes/{id}/attach` | `{"vm": "<uuid>", "force": false}` → 202. The reconciler connects on the VM's host and hot-plugs; poll until `attached.vm` matches. 409 if a *running* VM still has it, unless `force`; **409** `volume is being deleted` for a volume with `deleting: true` |
| POST | `/v1/volumes/{id}/detach` | 202; poll until `attached` is absent |

Volume:

```json
{"id": "7c0a…(32 hex)", "name": "pvc-1b2c", "server": "https://10.22.0.1:443",
 "traddr": "10.22.0.1", "port": "44002", "bytes": 5368709120,
 "perf": {"bs": "4k", "iodepth": 1, "seconds": 5, "write_lat_us": 187.5, "write_iops": 5000.0, "write_mib_s": 19.5},
 "attachTo": "<vm uuid>", "attached": {"vm": "<vm uuid>", "host": "timesink", "target": "vdc"},
 "message": null, "deleting": false}
```

`attachTo` is the wish, `attached` what the reconciler did. Inside the guest
the disk is `/dev/disk/by-id/virtio-<first 20 chars of id>` (virtio-blk
serials are 20 bytes). A VM that is deleted while holding a volume just
drops it: the volume survives, unattached. `perf` is ABE's fio
characterization when it ran one.

`GET /healthz` is unauthenticated and returns `ok`.
