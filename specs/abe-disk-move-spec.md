# ABE 0.7.0 / xdebbie 0.17.0: move a volume from one host's ABE to another

Status: draft, not started. Written 2026-11-14. Baseline: abe `e598808` (0.6.0 plus the LVM watchdog rework), xdebbie `8180a6d` (0.16.7). Intended home: `xdebbie/specs/abe-disk-move.md`.

## 0. Read this first

### Why

On 2026-11-14 the w4 cluster lost workers and then quorum while its disks were spread over gram and timesink. There was no way to get a volume off a host: ABE can create, clone, seal, export and delete, but not relocate, and `xdctl vm migrate` refuses ABE-backed VMs. The only option was to rebuild machines. We want one command that moves a volume's data to another host's ABE and leaves everything that refers to the volume working.

### The two changes

1. **ABE learns three small things**: create a volume under a caller-supplied id without exporting it; park a volume (unexport it but keep the data); unpark it. ABE does not copy anything and never talks to another ABE.
2. **xdebbie gets a move job**: a record in the store, driven by the leader's reconcile loop, with the copy run by the agent on the destination host. `xdctl` starts, lists, aborts and finalizes moves.

### The rule everything else follows from

**A volume's id never changes, and at no moment are two exports of the same id live.**

- The id is the LV name, the NVMe NQN, the namespace uuid behind `/dev/disk/by-id/nvme-uuid.<id>`, the libvirt disk source, and the Kubernetes PV `volumeHandle` (immutable). Keeping it means no domain XML, PV or record other than `server`/`traddr`/`port` has to change; `refresh_endpoints` already copes with a volume that answers from a different address.
- Two live exports with one NQN and one namespace uuid look like two paths to the same namespace. A host connected to both would treat them as multipath and split writes between two different disks. So the destination copy stays unexported until the source is parked.

### Order of work

ABE first (self-contained, testable with curl and `make e2e`), then xdebbie.

### Working rules (same as the earlier specs)

- Single-file ABE layout; new code next to what it touches.
- rustfmt-clean, `cargo test` green, `tests/lint-links.sh` green.
- `make e2e` on gram is the acceptance test for ABE; `tests/abe/run.sh` for xdebbie.
- Additive: existing endpoints and records keep their behaviour.
- Conventional commits, one concern per commit, CHANGELOG entries.

### Out of scope (don't build)

- **Live moves.** v1 needs the VM shut off or the volume detached. A later version can use libvirt `blockcopy --pivot`, but that needs two exports during the mirror and therefore an ABE rename/adopt operation. Not now.
- Draining Kubernetes pods. A CSI volume that is attached is refused, not drained.
- Preserving thin sharing. A moved clone becomes a full, independent thin volume on the destination (`origin` is cleared).
- Moving sealed bases. Each ABE keeps its own.
- ABE-to-ABE transfer, ABE as NVMe initiator, progress reporting, bandwidth limits.
- `xdctl host evacuate`. Build it on top of this later.

## 1. ABE

### 1.1 `GET /configure?id=<id>&bytes=N[&name=<name>]&export=false`

Create a volume whose id is chosen by the caller, blank and not exported.

- `id`: exactly 32 lowercase hex characters, else 400.
- Requires `export=false` and `bytes`; `id` without them is 400. (An exported create with a chosen id is not needed and would invite the two-exports mistake.)
- Always raw: no GPT, no fio, no trim.
- Tags the LV `abe_importing` and, when given, `abe_name=<name>`.
- Reply: `{"id", "bytes", "dev": "/dev/<vg>/<id>", "importing": true, "existing": false}`. `dev` is the local LV path; the caller is on the same box.
- Idempotent: the same `id` again, with the LV tagged `abe_importing` and the same size, returns it with `"existing": true`. Any other existing LV with that id is 409.
- 507 when the pool cannot hold it, as today.
- An `abe_importing` volume is skipped by the startup re-export pass and refused by `GET /clone` (409).

### 1.2 `GET /id/<id>` on an importing volume

Unchanged call, new meaning: exporting an `abe_importing` volume is what completes the import. ABE removes the tag, picks a port, exports and fences to the caller as usual.

### 1.3 `GET /park/<id>` and `GET /unpark/<id>`

- `park`: unexport the volume, close its fence port, keep the LV. Tag it `abe_parked` and move `abe_name=<n>` to `abe_parked_name=<n>` so a later `?name=` lookup does not find the parked copy. Idempotent. 409 on a sealed base.
- A parked volume is skipped by the re-export pass, and `GET /id/<id>`, `GET /clone/<id>` and `?name=` creates do not return it (409 / not found). `DELETE /id/<id>` removes it as usual.
- `unpark`: reverse the tags; the volume is exportable again with `GET /id/<id>`. Used only by abort.
- `GET /volumes` gains `"importing"` and `"parked"` booleans. `GET /version` gains the feature `move`.

### 1.4 Tests

- Unit: id validation; tag transitions for importing, park, unpark; re-export pass skips both states; `?name=` ignores parked.
- `tests/e2e.sh`: create with id and `export=false`, write a pattern to `dev`, export, connect, read it back; park, confirm the port is closed and `GET /id` is 409; unpark and export again; restart abe between steps and confirm neither state is exported.

## 2. xdebbie

### 2.1 Record

A `Move` in the store, keyed by its own uuid:

| Field | Meaning |
|---|---|
| `id` | the move's uuid |
| `volume` | ABE volume id being moved |
| `target` | `VmDisk { vm, role }` or `Volume { name }` |
| `from`, `to` | host names |
| `phase` | see 2.2 |
| `message` | last error or note |
| `started`, `updated` | timestamps |

At most one non-terminal move per volume.

### 2.2 Phases

`Pending → Preparing → Copying → Verifying → Switching → Done`, plus `Failed` and `Aborted`. `Done` still holds the parked source until `Finalized`.

| Phase | What happens | Where |
|---|---|---|
| Preparing | Run the checks in 2.3. Create the destination volume with `?id=&bytes=&name=&export=false`. | leader, calling `to`'s ABE through `to`'s agent |
| Copying | Export the source to the destination host and connect it there. Copy source device to the local `dev` path. | agent on `to` |
| Verifying | Compare the two devices. | agent on `to` |
| Switching | Disconnect the source on `to`. Park the source. Export the destination (`GET /id/<id>` on `to`'s ABE). Rewrite the record: `server`, `traddr`, `port`; `origin = None`; `populated` unchanged. | leader + both agents |
| Done | Normal reconciliation resumes; the VM may start or the volume may attach. | |
| Finalized | `DELETE /id/<id>` on `from`'s ABE. Explicit, never automatic. | leader |

Parking the source is the point of no return. Before it, failure or abort deletes the destination volume and leaves the source exactly as it was. After it, the job only rolls forward; if exporting the destination fails, the phase stays `Switching` with a message and is retried every tick. An operator can still recover by hand with `unpark`.

### 2.3 Checks before anything is created

- Both hosts are healthy and their ABEs report the `move` feature.
- `to` is not `from`, and `to`'s ABE has no volume with this id (other than our own importing one).
- Destination pool free space covers the source's `used_bytes` plus 10%.
- For a VM disk: the VM's power intent is Off and libvirt reports `shut off`. A `paused` domain is refused.
- For a standalone volume: `attach_to` is empty and no attachment is observed.
- **No host has the volume connected.** Ask every host's agent whether the NQN is connected, and refuse while any is. This catches stale sessions; on 2026-11-14 gram held live sessions to root disks of VMs running on timesink.

### 2.4 The copy (agent on the destination host)

New agent call `POST /agent/copy-volume { volume: Disk, dev: String }`, long-running like `provision`.

1. `GET /id/<id>` on the source ABE from this host, so the fence opens to this host's address. Connect and wait for `/dev/disk/by-id/nvme-uuid.<id>`.
2. `qemu-img convert -n -f raw -O raw --target-is-zero -S 64k <source> <dev>`. A new thin LV reads as zeros, so only written regions are copied. If the destination ABE has no thin pool, drop `--target-is-zero`.
3. Flush the destination.

Verify is a second call, `POST /agent/verify-volume`, running `qemu-img compare -f raw -F raw <source> <dev>`.

Both calls take the per-volume keyed lock. They are restartable: a copy that is asked for again while one is running waits for it; one asked for after a crash starts over, which is safe because the destination is scratch until verified. Always disconnect the source session on exit, success or not.

### 2.5 Interlocks

While a move for a volume is in a non-terminal phase:

- `xdctl vm start` on the owning VM is 409, and the reconciler does not start it or connect its disks.
- Volume attach requests for it are 409.
- `xdctl vm rm` and volume delete are 409; abort the move first.

### 2.6 API and CLI

| CLI | API |
|---|---|
| `xdctl disk move <vm>[:root\|:data] --to <host>` | `POST /v1/moves` |
| `xdctl volume move <name\|id> --to <host>` | `POST /v1/moves` |
| `xdctl move ls`, `xdctl move describe <id>` | `GET /v1/moves[/<id>]` |
| `xdctl move abort <id>` | `DELETE /v1/moves/<id>` (409 once `Switching` has parked the source) |
| `xdctl move finalize <id>` | `POST /v1/moves/<id>/finalize` |

`xdctl vm describe` and `volume describe` show an active move.

### 2.7 Leader change

The job lives in the store, so a new leader picks it up from its phase. `Preparing`, `Copying` and `Verifying` are safe to repeat. `Switching` steps are each idempotent (disconnect, park, export, record write) and are re-run in order.

### 2.8 Tests

- `tests/abe/run.sh` with two fake ABEs: happy path for a VM root disk and for a standalone volume; abort during `Copying` leaves the source untouched and removes the destination; a failure injected after park is retried to `Done`; a stale connection on a third host is refused; an attached volume is refused; a leader restart in each phase completes.
- On gram and timesink: move a stopped test VM's root disk each way, boot it, check a file written before the move; move a detached PV, re-attach it, check its contents.

## 3. Things to settle before building

1. **Does abe-csi go through xdebbied's volume API for every volume, or does anything talk to an ABE directly by address?** If the latter, that component needs the new `server` after a move.
2. **Where do stale cross-host sessions come from?** `Libvirt::start` calls `ensure_connected(vm)` with no host, and agent calls fall back to running locally when a host has no agent. If the leader can connect a remote VM's disks on its own box, fix that first; otherwise check 2.3 will refuse most moves.
3. **Should `Done` auto-finalize after N hours?** This spec says never. A parked source costs pool space on the host being emptied.
4. **Verification cost.** `qemu-img compare` reads both volumes in full. Fine for 20 GiB roots; reconsider for large data volumes (a checksum of allocated extents only would be cheaper).
5. **Fence and `abe_client`.** After the switch the destination is fenced to whichever host first asks to connect. Confirm that is always the VM's host and never the leader.
