# ABE Live Volume Move — Plan / Implementation Spec

Version 0.1 (draft for review) · November 13, 2026 · Owner: Brian Matheson · Implementer: Goose
Targets: `abe` 0.6.0 (from 0.5.2) · `xdebbied`/`xdctl` 0.17.0 (from 0.16.6) · `abe-csi` 0.3.5 (no code change, new e2e)
Written against: abe `2db360f`, xdebbie `960dc23`, abe-csi `f84bad2`.

This is the "separate spec" that `abe-volume-hardening.md` deferred ("Multi-host attach and ABE live migration"). Multi-host attach landed in 0.16.5 (agents); this is the rest of the storage half.

---

## 0. Read this first

### What it does

Move a volume's data from one ABE to another **while something is using it**, with the user of the volume seeing a short I/O pause and nothing else. Same volume id, same NQN, same `/dev/disk/by-id/nvme-uuid.<id>` on the consumer, same virtio disk in the guest, same PV in Kubernetes.

It covers "VM storage" and "PVC storage" with one mechanism, because in this stack they are the same thing one layer down: an NVMe/TCP namespace connected on a hypervisor and handed to QEMU as a block device. A pod's PVC is a hot-plugged virtio disk of its worker VM. The mechanism sits below all of that, in the kernel NVMe layer, so it also works for a consumer that is not QEMU at all (a host mount, a container on a bare host using `nvme connect`).

### The idea in five lines

1. **Pre-copy.** The source ABE takes thin snapshots of the volume and ships them to the destination ABE: first every mapped block, then only what changed (`thin_delta` between snapshots), round after round until the delta is small.
2. **Second path.** The consumer connects to the destination's export of the *same NQN and namespace UUID*. Linux NVMe multipath adds it as a second path of the existing device. The destination advertises it ANA `inaccessible`, so no I/O goes there yet.
3. **Freeze.** The source flips its path to `inaccessible` and disables the namespace (which drains every in-flight command). With every path inaccessible but the controllers alive, the consumer's kernel queues I/O instead of failing it.
4. **Final delta and flip.** The source ships the last delta; the destination flips its path to `optimized`. The kernel gets an ANA change event and replays the queued I/O down the new path.
5. **Clean up.** The consumer drops the old controller; the source keeps its now-frozen copy until a background scrub proves the destination matched it at the moment of the flip, then deletes it.

### What is proven and what is not

Checked on these boxes today (read-only):

| Fact | timesink | gram |
|---|---|---|
| Kernel | 6.12.115+deb13.1 | 7.3.3+deb14 |
| `nvme_core.multipath` | Y | Y |
| `CONFIG_NVME_MULTIPATH` | y | y |
| ABE volumes already appear as multipath heads with one path (`nvme1n1` + `nvme1c1n1`) | yes; ANA group 1 `optimized` | yes; ANA state not inspected |
| nvmet configfs has `ports/<p>/ana_groups/1/ana_state`, `namespaces/1/ana_grpid`, `subsystems/<id>/attr_cntlid_min`/`max` (1 / 65519), `attr_serial` | yes | not inspected |
| Live controller: `cntlid=1`, `ctrl_loss_tmo=off`, `reconnect_delay=10` | yes | not inspected |
| `thin_delta`, `thin_dump` (thin-provisioning-tools 1.1.0) | yes | yes |
| `CONFIG_DM_CLONE` | **not set** | m |
| QEMU / libvirt | 10.0.13 / 11.3.0 | 11.1.3 / 12.9.0 |
| Head attribute `delayed_removal_secs` | not checked | present, 0 |

Also measured: gram's wired NIC reports 2500 Mb/s; three pings to `10.22.0.1` over `wg-flock` averaged 1.4 ms.

**Not proven: everything about how the kernel behaves during steps 2–4.** The design rests on my understanding of the Linux NVMe host and target code, not on a test:

- a second target exporting the same NQN + namespace UUID joins as a path, provided its controller ids don't collide with the first target's;
- I/O is requeued, not failed, while all paths are `inaccessible` and at least one controller is live;
- writing `0` to a namespace's `enable` returns only after every request that references it has completed, including a write still waiting for its data from the network;
- an ANA change event makes the host replay the requeued I/O on the newly optimized path.

Nothing has been run. **Phase 0 exists to prove or kill these on both kernels before any Rust is written** (section 2). If they fail, section 9 has the fallbacks.

### Order of work

| Phase | What | Repo | Leaves behind |
|---|---|---|---|
| 0 | Kernel spike by hand: configfs, nvme-cli, fio, one scratch VM | none | `milestones/…-abe-live-move.md` with the results and the go/no-go |
| 1 | Thin-delta engine: snapshots, `thin_dump`/`thin_delta` parsing, range reader | abe | unit tests; `abe move-plan <id>` debug subcommand |
| 2 | Offline move end to end: `/move`, `/receive`, round stream, commit, retained source, scrub | abe | two-host e2e for a volume nobody has connected |
| 3 | Live cutover: standby export, ANA, cntlid ranges, freeze, rollback, crash recovery | abe | two-host e2e under `fio --verify` |
| 4 | Orchestration: per-path NVMe helpers, agent ops, reconciler state machine, API, `xdctl` | xdebbie | `xdctl volume move`, `xdctl vm move-storage` |
| 5 | Real workloads and fault injection; docs; rollout | all | milestone log, `docs/storage.md` section |

Phases 1 and 2 are useful on their own (cold move of a stopped VM's disks) and carry no kernel risk. Phase 3 is where the risk is. Rough size, as an estimate only: abe grows by something like 1,500–2,000 lines; xdebbie by 600–900.

### Working rules

Same as earlier milestones: version bump on every installed rebuild, host changes ship in the `.deb`, `cargo fmt`/`clippy -D warnings`/`cargo test` clean, new state fields `#[serde(default)]`, API additions only, milestone log per fix, same commit trailer convention. Two additions:

- `src/main.rs` in abe is 3,817 lines. Put this feature in new modules (`src/thin.rs`, `src/mover.rs`, `src/receiver.rs`) rather than growing it.
- New endpoints use real verbs (`POST`/`PUT`/`DELETE`). The existing GET-that-mutates routes stay as they are.

### Out of scope

- Moving a running VM's CPU and memory. timesink is AMD and gram is Intel; QEMU cannot live-migrate between them whatever the storage does. This spec makes storage follow a VM, or leave a box, independently of where the VM runs.
- Keeping two live copies (replication, HA). The second copy exists only for the duration of the move.
- Thick (non-thin-pool) volumes, live. There is no dirty tracking for them. They get the offline path only, as a full copy.
- Deduplicating a root disk against the destination's own base image. The destination copy is a plain thin volume; `Disk.origin` is cleared. (Section 10, question 4.)
- Sealed bases, automatic rebalancing, moving between pools on one ABE, non-Linux initiators.

---

## 1. Why this mechanism

| Option | How | Why not (or why) |
|---|---|---|
| **Thin-delta pre-copy + ANA path flip** (this spec) | Above | Source stays whole and authoritative until one atomic flip, so abort is free and no moment depends on both boxes. Consumer-agnostic. Needs only LVM thin, nvmet and native multipath, present on both boxes. The unproven part is kernel behavior, which a spike can settle cheaply. |
| QEMU `blockcopy` + pivot | Hypervisor connects a second volume; QEMU mirrors and pivots | Needs no ABE or kernel novelty, and is the best fallback for VM consumers. But it only works while a domain is running, only for QEMU consumers, reads the whole virtual size through the hypervisor, and the destination must have a *different* NQN (same NQN would join as a multipath path), which means splitting xdebbie's volume id from the ABE id, while the PV `volumeHandle` is immutable. |
| dm-clone on the destination (post-copy) | Flip first, hydrate in the background | Instant cutover, no convergence problem. But `CONFIG_DM_CLONE` is not set in timesink's kernel, so it could only ever move volumes *to* gram. And until hydration finishes the volume lives on two boxes: losing either loses data, and there is no abort after the flip. |
| dm-mirror / md-raid1 under the export | Mirror local LV to a remote leg, then drop the local leg | Means swapping the device under a live nvmet namespace, or hijacking an LVM-owned dm table. Fragile, and LVM stops understanding its own LV. |
| Stop, copy, start | — | The thing we are trying not to do. It is, though, exactly Phase 2. |

---

## 2. Phase 0: the spike

By hand, no ABE changes, on a scratch thin LV on each box. Every step runs with **timesink as consumer (6.12) and again with gram as consumer (7.3)**. Workload throughout: `fio --rw=randwrite --verify=crc32c --verify_backlog=1024 --direct=1` on the consumer's head device.

| # | Question | Pass |
|---|---|---|
| S1 | Target B exports the same NQN and `device_uuid` as target A, with `attr_cntlid_min` set above A's range and (if writable) A's `attr_serial`. Does `nvme connect` to B add a second path to the existing head? | `nvme list-subsys` shows two controllers under one subsystem; one `nvmeXnY` |
| S1b | Same without the cntlid range | Rejected with "Duplicate cntlid" (confirms the range is needed) |
| S2 | B's port ANA `inaccessible` from before the connect; then A's `inaccessible` too, for 30 s | fio stalls, zero errors, `dmesg` shows requeuing; flipping B to `optimized` resumes within a second |
| S3 | Under load, with B inaccessible: `echo 0 > …/namespaces/1/enable` on A | fio stalls, zero errors; head device survives; the write returns only when A is drained |
| S3b | Same with `tc netem` delaying the consumer→A link so a write is mid-data when the disable starts | The disable blocks until that write finishes; nothing is written to A's LV afterwards (`blktrace` or a snapshot comparison) |
| S4 | Rollback: re-enable A's namespace, A back to `optimized` | fio resumes on A |
| S5 | S2–S4 with a scratch VM on top (virtio-blk, `cache=none`), fio inside the guest | No guest I/O errors, no remount-ro; record the stall the guest saw |
| S6 | `nvme disconnect -d nvmeN` of A's controller after the flip | Head and by-id link intact, fio unaffected |
| S7 | `reserve_metadata_snap` + `thin_delta` on a live pool against a known write pattern, including a discard and a pool whose chunk size differs from the other box's | Reported ranges equal the pattern exactly |
| S8 | Time `lvcreate -s`, `thin_delta` and `lvchange` on the real pools | Numbers for the freeze budget in 4.5 |

Go/no-go: S1, S2, S3, S3b and S5 must pass on both kernels. Write the results into the milestone log either way.

---

## 3. Background: what exists today

**ABE** (`src/main.rs`). A volume is a thin LV named by a 32-hex id; the id is the NQN and (dashed) the namespace `device_uuid`. `export()` creates the subsystem (`attr_allow_any_host`), namespace 1, and one nvmet port per volume in 23000–23999. `fence_open()` opens that port to one client address, the caller of `/configure`, `GET /id` or `/clone`. Durable state is LV tags (`abe_port`, `abe_name`, `abe_base`, `abe_sealed`, `abe_client`); `reexport_one()` rebuilds exports from them at startup. `OPS` is one global mutex around LVM and configfs work. ABE is a target only: it never runs `nvme`, and it has no HTTP client (`Cargo.toml` has axum, rustls, no reqwest). On these boxes it serves mTLS with the xdebbie box certificate, which carries clientAuth as well.

**xdebbie.** `Nvme::connect`/`connected`/`disconnect` in `crates/xdebbied/src/abe.rs` are keyed by NQN alone: connect is a no-op when the NQN is connected, disconnect is `nvme disconnect -n <nqn>`. `Disk` and `Volume` carry `server`, `traddr`, `port`. `refresh_endpoints` asks `GET /id/<id>` for a disk that is not connected and rewrites the record. `Virsh::migrate` bails for `storage = abe`. Agent dispatch (`agent.rs`) runs host-local work on the box that owns the VM.

**abe-csi.** A thin wrapper over scripts that call xdebbied (`POST /volumes/<id>/attach`); the guest finds the disk by virtio serial. `volumeHandle` is the volume id.

---

## 4. ABE 0.6.0

### 4.1 Invariant

> At every instant, at most one ABE exports the real NQN with its namespace enabled and its port ANA `optimized`. Which one is decided by a single durable bit: the `abe_recv` tag on the destination's LV. While the tag is there the source owns the volume; the `lvchange --deltag` that removes it is the commit point.

Every recovery rule in 4.7 follows from this.

### 4.2 New LV tags

| Tag | On | Meaning |
|---|---|---|
| `abe_move=<dst host:port>` | source | A move is in progress to there |
| `abe_frozen` | source | Namespace disabled for the final delta; do not export without asking the destination |
| `abe_moved_to=<dst host:port>` | source | Committed. Retained copy: never exported, reported in `/volumes`, deleted by `DELETE /id/<id>` |
| `abe_recv=<src host:port>` | destination | Incoming. Never exported `optimized`; `GET /id` gives a standby export |
| `abe_gen=<n>` | both | Move generation, for controller id ranges (4.4). Absent = 0 |

Snapshots made for a move are named `<id>.mv<round>`; `is_volume_id` already ignores them. They are removed on commit, abort, and at startup when their volume has no `abe_move`.

### 4.3 Thin-delta engine (`src/thin.rs`)

- `mapped(id)`: every mapped range of a thin device, from `thin_dump -m --dev-id`.
- `delta(a, b)`: ranges that differ between two thin devices, from `thin_delta -m --snap1 --snap2`. `different` and `right_only` are data to send; `left_only` is a range that became unmapped and must read as zeros on the destination.
- The metadata snapshot shows committed mappings only. S7 checks whether reserving it commits first; if not, flush the device before reserving.
- Both need the pool's metadata snapshot (`dmsetup message <pool>-tpool 0 reserve_metadata_snap` … `release_metadata_snap`). A pool has one, so a per-pool mutex guards it; it is always released, including on error and at startup.
- Ranges are in pool chunks of the *source*. Convert to bytes before they leave this module.
- XML is parsed as a stream; a fully mapped large volume must not be held in memory as one document.
- `thin-provisioning-tools` becomes a hard `Depends` of the `.deb`. `/version` lists the feature `move` only with a thin pool, the tools present, and TLS on.

### 4.4 Controller ids, serial, size

Two targets exporting one NQN to one host must not hand out the same controller id. Every export sets `attr_cntlid_min = gen×1000 + 1` and `attr_cntlid_max = (gen+1)×1000`, where `gen` is the `abe_gen` tag. Volumes exported before 0.6.0 are generation 0 and keep the kernel default range, which starts at 1, so the first move goes to generation 1 without touching the live source export. The destination uses `(source gen mod 60) + 1`.

The destination copies the source's `attr_serial`, and creates the LV with **exactly** the source's byte size. If its VG's extent size cannot produce that size, it answers 409; a namespace whose paths disagree about capacity is not something to find out about in production.

### 4.5 The move, step by step

Source = S, destination = D, consumer = the host with the volume connected (possibly S or D itself).

**Copy (no effect on the consumer beyond snapshot overhead)**

1. Client: `POST S/move/<id> {"to": "https://D:443"}`. S checks: thin, not a sealed base, no move running, pool has headroom for snapshots. Tags `abe_move`, answers 202.
2. S → D: `PUT /receive/<id> {bytes, name, gen, serial}`. D creates the LV with `abe_recv`, `abe_name` and `abe_gen`; no export. Idempotent. D answers 409 if it holds a retained copy of the same id (moving back before the old copy was deleted).
3. Round 0: S snapshots the volume (`<id>.mv0`), reads every mapped range from the snapshot with `O_DIRECT`, and streams it to `PUT D/receive/<id>/round/0`. All-zero chunks are skipped (the destination is unmapped there).
4. Round n: snapshot `<id>.mv<n>`, `delta(mv<n-1>, mv<n>)`, stream it, drop `mv<n-1>`. Ranges that became unmapped, and chunks that became zero, are sent as zero records.
5. When a round's dirty bytes would take less than the freeze target to send at the measured rate, phase becomes `ready`. S keeps doing rounds every few seconds to stay close. If the write rate outruns the copy rate, phase is `not_converging` and says so; the client decides whether to commit anyway within `max_freeze_ms`.

**Standby (consumer gains a second path)**

6. Consumer: `GET D/id/<id>`. For an incoming volume D exports the real NQN and UUID on a port whose ANA group 1 is `inaccessible`, fenced to the caller, and answers with `"standby": true`. Consumer runs `nvme connect` to it.

**Commit (the freeze)**

7. Client: `POST S/move/<id>/commit`. S asks D whether the consumer (S's `abe_client`) has an established connection on the standby port (`established_peers`, which D already has for adoption). No standby and S's own port has a peer → 409 `standby path not connected`. No peer on either → nobody is using the volume; proceed (this is the offline move).
8. S: port ANA → `inaccessible`; namespace `enable` → 0, under a timeout; tag `abe_frozen`. From here S can never complete another write to this volume.
9. S: `delta(mv<n>, live volume)`, with no snapshot, since the live volume has no writers. Stream it as the final round. D applies it, flushes the device, and (when scrubbing) snapshots the result as `<id>.moved`.
10. S → D: `POST /receive/<id>/activate`. D removes `abe_recv` (**commit point**), flips the port to `optimized`, answers 200.
11. S: tags `abe_moved_to`, removes `abe_move`/`abe_frozen`, unexports, drops its snapshots. Answers the commit call with `{moved, frozen_ms, traddr, port}`.
12. Consumer: disconnects the old controller **by device** (`nvme disconnect -d nvmeN`), never by NQN.

If anything in 8–9 fails, or `max_freeze_ms` (default 5000) passes before step 10 is sent, S rolls back: namespace re-enabled, ANA `optimized`, tags cleared, phase `ready` again with the error recorded. If step 10 was sent and the answer is missing, S does **not** roll back; see 4.7.

**Freeze budget**: steps 8–10 are two configfs writes, one `thin_delta`, the final delta over the wire, one flush, one optional `lvcreate -s`, one `lvchange`, one HTTP round trip. S8 of the spike puts numbers on the LVM parts. The target is under a second for a volume that is not being hammered; this is a goal to measure, not a promise.

**Scrub (after)**

13. S's retained LV is immutable from step 8; D's `<id>.moved` is the same instant. Both sides hash them in fixed extents; S compares. Match → D drops the snapshot, and the retained copy may be deleted. Mismatch → both are kept, the move reports `scrub: mismatch` with the first differing offset, and nothing is deleted automatically. This is the net under the whole feature: a copy bug shows up while the original still exists.

### 4.6 API

Source:

| Method | Path | |
|---|---|---|
| POST | `/move/<id>` | `{to, rate_mib_s?, max_freeze_ms?, scrub?}` → 202 status |
| GET | `/move/<id>`, `/moves` | `{phase, round, copied_bytes, mapped_bytes, last_dirty_bytes, last_round_ms, est_freeze_ms, standby, frozen_ms?, scrub?, error?}` |
| POST | `/move/<id>/commit` | Blocks; 200 moved, 409 not ready / no standby, 504 rolled back |
| DELETE | `/move/<id>` | Abort before commit: drops snapshots, tells D to drop the incoming volume |
| POST | `/move/<id>/resolve` | `{as: "source" \| "destination"}`: operator override for 4.7 |

Destination:

| Method | Path | |
|---|---|---|
| PUT | `/receive/<id>` | Create the incoming volume |
| PUT | `/receive/<id>/round/<n>` | Framed stream of `write(offset, len, data)` and `zero(offset, len)` records, closed by a digest. Absolute offsets, so replaying a round is harmless |
| GET | `/receive/<id>` | `{state, round, standby_peers}` |
| POST | `/receive/<id>/activate` | The commit point |
| DELETE | `/receive/<id>` | Drop an incoming volume; refused once active |

Changed: `GET /id/<id>` on an incoming volume gives the standby export; on a retained copy it answers **410** with `{moved_to}` so a stale client can follow. `/volumes` gains `incoming` and `moved_to`. `DELETE /id/<id>` on a retained copy deletes just that copy.

Zero records need care: a discard that covers only part of a destination chunk is ignored by dm-thin and would leave old data readable. D discards whole destination chunks only and writes real zeros at the edges.

ABE gains an HTTPS client (reqwest on rustls) using `ABE_TLS_CERT`/`KEY` as identity and `ABE_TLS_CA` as the only root. With `ABE_TLS=off`, move and receive are disabled: volume contents do not cross the wire in clear, and nobody unauthenticated gets to write a volume. `ABE_MOVE_PEERS` (optional address list) narrows who may be a destination or a source. `ABE_MOVE_MIB_S` caps the bulk copy so it does not starve the NVMe/TCP traffic sharing the underlay.

### 4.7 Crashes and partitions

| What dies, when | State afterwards | Recovery |
|---|---|---|
| S, during copy | `abe_move`, no `abe_frozen` | Re-export normally. Move is `failed`; client restarts it. (Resuming from the last snapshot is a later refinement.) |
| D, during copy | `abe_recv` | D does not export it. S's round fails; move `failed`; abort drops the incoming LV |
| Network, during copy | — | Round fails, retried with backoff, then `failed`. Consumer unaffected |
| Anything, during freeze before step 10 is sent | S frozen, D incoming | S rolls back (or, if S itself died: at startup sees `abe_frozen`, asks D, hears `receiving`, re-exports and tells D to drop it) |
| Step 10 sent, answer lost | Unknown to S | S stays frozen and keeps asking `GET D/receive/<id>` until it gets a definite answer: `active` → finish step 11; `receiving` → roll back and tell D to drop it. **Never guess.** The consumer's I/O stays queued meanwhile: a stall, not a fork |
| S, after D activated | D owns it; S has `abe_frozen` | S at startup asks D, hears `active`, tags `abe_moved_to`, does not export |
| D, after activating | No `abe_recv` | Ordinary volume; ordinary re-export at startup |
| Consumer host | — | Nothing special: on return it asks `GET /id` wherever its record points; a 410 redirects it |

A box that stays unreachable during the ambiguous case leaves the volume stalled until someone runs `/move/<id>/resolve`. That is deliberate: stalled is recoverable, forked is not.

### 4.8 Tests

Unit: `thin_dump`/`thin_delta` parsing against captured output; range conversion between unequal chunk sizes; zero-record edge handling; stream framing; the recovery table of 4.7 as a pure function of (tags here, state there).

Two-host e2e (`tests/e2e-move.sh`, `SRC=` `DST=`, run on the consumer). Two ABEs on one box cannot share an NQN in one nvmet, so this cannot be a single-host test and will not run in CI; CI gets the unit tests and a fake peer.

1. Offline move; full-device compare.
2. Live move under `fio --verify`; zero errors; full compare after quiescing; report `frozen_ms`.
3. Abort mid-copy; source untouched, destination clean.
4. Forced rollback (`max_freeze_ms=1` under heavy writes); fio continues on the source.
5. Commit without a standby path while connected → 409, nothing changed.
6. `kill -9` of each ABE at each phase; `wg-flock` down for 30 s during copy and during freeze; the invariant holds and recovery matches 4.7.
7. Unequal pool chunk sizes; discards during the move; a volume that is a clone of a base; move there and back.
8. Scrub detects a deliberately corrupted destination block.

---

## 5. xdebbie 0.17.0

- **Per-path NVMe helpers** (`abe.rs`): `connected_path(nqn, traddr, port)`, `connect` that no longer treats "NQN is connected" as done, `disconnect_path` by controller device. `disconnect(nqn)` stays, for teardown of every path. This is a prerequisite and worth landing first, on its own.
- **Model**: `Disk.moving` / `Volume.moving: Option<Move>` (`{to, phase, message}`, `#[serde(default)]`). `Disk.origin` is cleared when a root disk moves.
- **Agent ops**: `volume-standby` (call `GET dst/id/<id>` from the consumer so the fence opens to it, connect, confirm two paths) and `volume-drop-path`. Both run where the VM runs, like every other NVMe operation.
- **Reconciler**: one state machine per moving volume: `Requested → Copying → Ready → Standby → Committing → Moved → Scrubbing → Done`, each step idempotent. `Moved` rewrites `server`/`traddr`/`port` before anything else. `refresh_endpoints` leaves a moving disk alone and follows a 410. A VM's disks move one at a time.
- **Capacity**: the destination must pass `abe_fit` for the volume's size; an in-flight move counts in the pending ledger; the source's space is freed only when the retained copy is deleted.
- **API and CLI**: `POST /v1/volumes/{id}/move {host}`, `POST /v1/vms/{id}/move-storage {host, volumes?}`, `DELETE` of either to abort. `xdctl volume move <vol> --to <host>`, `xdctl vm move-storage <vm> --to <host> [--with-volumes] [--manual-commit]`, progress in `xdctl volume ls` / `vm ls` MESSAGE.
- **Tests**: `fake-abe.py` learns `/move` and `/receive`; reconciler tests drive the state machine through every failure of 4.7; a v0.16 state file still loads.

abe-csi needs nothing: the volume id, the guest's virtio serial and the PV are all unchanged. It gets one e2e: a pod writing checksummed data to a PVC while the volume moves under it.

---

## 6. What this does for the cluster today

All twelve PVC volumes and every w4 disk except the two new workers' roots sit on timesink's ABE. With this, plus a cold VM move (shut down, define on the other box, connect across the mesh, start: about a reboot's worth of downtime, and a separate small change to `Virsh::migrate`), a worker can be relocated in two independent steps: the VM cold, its storage live, in either order. Emptying a box of storage before maintenance becomes possible without stopping what uses it.

It does not make live VM migration between timesink and gram possible. Nothing will, short of matching CPUs.

---

## 7. Rollout

1. Phase 0 results reviewed; go/no-go.
2. abe 0.6.0 to gram, then timesink. The restart re-exports and adopts live connections, as 0.5.x already does. No existing export changes: generation 0 keeps the default controller id range.
3. xdebbie 0.17.0 to both.
4. First moves, in order: a scratch volume with fio; a scratch VM's root; one small PVC with a checksum loop in its pod; one worker's root. Stop at the first surprise.

---

## 8. Risks

| Risk | Consequence | Mitigation |
|---|---|---|
| Kernel does not behave as assumed on 6.12 or 7.3 | No transparent cutover | Phase 0, before any code; fallbacks in 9 |
| A copy bug | Silent corruption | Scrub against the frozen source before the source is deleted; `fio --verify` e2e; mismatch keeps both copies |
| Lost write at the freeze | Silent corruption | Namespace disable as the drain, not an in-flight counter (a write can be past nvmet's ANA check and still waiting for its data); S3b tests exactly this |
| Guest reacts badly to a multi-second stall | Remount-ro, timeouts in the workload | `max_freeze_ms` with rollback; S5 measures; default budget well under typical guest timeouts |
| Snapshot overhead on the source during the copy | Higher write latency; pool fills | Headroom check before starting; rate cap; snapshots dropped every round |
| Stuck in the ambiguous state | Volume stalled until an operator resolves | By design; `/resolve`; alert in `xdctl` |
| Both boxes share one 2.5 GbE link with live NVMe/TCP | Copy starves guests | `ABE_MOVE_MIB_S` |

---

## 9. If the spike fails

- **S3 fails (namespace disable causes I/O errors at the head) but S2 passes**: fence the source by unlinking the subsystem from its port instead. That tears the controllers down, which multipath treats like a pulled cable. Rollback becomes slow (the host reconnects on its 10 s timer) but the cutover still works.
- **S1 or S2 fails on one kernel**: that box cannot be a *consumer* of a live move; it can still be a source or destination. timesink's kernel is the older one, so check whether a backport kernel fixes it before giving up.
- **S1 or S2 fails on both**: drop Phase 3. Keep Phases 1–2 (offline move), and do the live case for VM consumers with QEMU `blockcopy`, accepting the id split described in section 1.

---

## 10. Decisions wanted before Phase 1

1. **Commit**: automatic once `ready` and the standby path is up (proposed default), or always an explicit step?
2. **Retained source copy**: delete as soon as the scrub passes (proposed), or keep for a fixed time?
3. **Scrub snapshot inside the freeze**: it costs one `lvcreate -s` of freeze time (S8 will say how much). On by default (proposed)?
4. **Root disks lose their thin-clone sharing** when moved (a 20 GiB root becomes a standalone volume holding its mapped blocks). Acceptable for v1?
