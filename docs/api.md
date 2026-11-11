# xdebbied REST API (v1)

Base URL `https://<xdebbied>:7443`. With `token_file` configured, every `/v1`
call needs `Authorization: Bearer <token>`. Bodies are JSON with camelCase
keys. Errors are `{"error": "..."}`.

## Hosts

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/hosts` | List hosts, with discovered `capacity` and `lastSeen` |
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
 "reservedMemoryMib": 4096, "reservedCpus": 2}
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
| POST | `/v1/volumes` | `{"name": "pvc-…", "bytes": N, "host": null}`. Synchronous: asks ABE and returns 201 with the id ABE assigned (200 if a volume with that name already exists, 409 if it is smaller than asked). 502 when ABE fails, 503 with no healthy ABE host |
| GET | `/v1/volumes/{id}` | |
| DELETE | `/v1/volumes/{id}` | 204; 409 while attached (detach first). Synchronous on ABE |
| POST | `/v1/volumes/{id}/attach` | `{"vm": "<uuid>", "force": false}` → 202. The reconciler connects on the VM's host and hot-plugs; poll until `attached.vm` matches. 409 if a *running* VM still has it, unless `force` |
| POST | `/v1/volumes/{id}/detach` | 202; poll until `attached` is absent |

Volume:

```json
{"id": "7c0a…(32 hex)", "name": "pvc-1b2c", "server": "http://10.22.0.1:80",
 "traddr": "10.22.0.1", "port": "44002", "bytes": 5368709120,
 "perf": {"bs": "4M", "seconds": 5, "write_mib_s": 940.0, "write_iops": 235.0, "write_lat_ms": 33.0},
 "attachTo": "<vm uuid>", "attached": {"vm": "<vm uuid>", "host": "timesink", "target": "vdc"},
 "message": null}
```

`attachTo` is the wish, `attached` what the reconciler did. Inside the guest
the disk is `/dev/disk/by-id/virtio-<first 20 chars of id>` (virtio-blk
serials are 20 bytes). A VM that is deleted while holding a volume just
drops it: the volume survives, unattached. `perf` is ABE's fio
characterization when it ran one.

`GET /healthz` is unauthenticated and returns `ok`.
