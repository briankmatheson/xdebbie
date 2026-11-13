# Milestone: ABE volume hardening (2026-11-13)

Spec: `abe-volume-hardening.md` (kept with the other specs; written against abe `892a98f`,
xdebbie `85993ce`, abe-csi `d3228ca`). Delivered as abe 0.4.0 (package now
named `abe-block-exporter`, HEAD `cb10127`), xdebbie 0.15.0 (this branch,
`abe-volume-hardening`, on top of `560ad90`) and abe-csi 0.3.5 (`d099a61`).

One row per change: what broke, what changed, which version, and the test
that proves it. Unit tests are `cargo test` names; `e2e` is
`abe/tests/e2e.sh` (root, real thin pool); `run.sh` is xdebbie's
`tests/abe/run.sh` (unprivileged, fake ABE 0.4.0, phase and check name).

## abe 0.4.0

| # | Issue | Fix | Version | Test that proves it |
|---|---|---|---|---|
| 1 | Nothing serialized mutations: two concurrent creates could take the same nvmet port index, and the only metadata about a volume was its export in configfs | One process-wide `OPS` lock around every mutation; durable LV tags `abe_port`, `abe_name`, `abe_base`, `abe_sealed` written with the LV and read back with `lvs -o lv_tags,lv_active` | abe 0.4.0 (`cb40440`) | `parses_lvs`, `parses_lvs_without_the_tag_columns`, `reads_tags`; e2e 3 "concurrency: 8 parallel creates, 8 distinct ports" |
| 2 | Ports only ever went up (`last_port_number() + 1`), formatted to 6 digits past index 999, and were never reused | Lowest free port in `ABE_PORT_MIN..=ABE_PORT_MAX` (default 44001..44999) from a configfs scan; 503 with a clear body when the range is exhausted | abe 0.4.0 (`41d07e3`) | `picks_the_lowest_free_port`, `scans_used_ports`; e2e 2 "port reuse: D took B's port", e2e 9 "port range exhausted" |
| 3 | Exports lived only in configfs, so a reboot or module reload silently lost every volume | Re-export every tagged LV at startup on its recorded `abe_port` (waiting `ABE_REEXPORT_WAIT` for the address), `GET /reexport` on demand, summary in `/utilization.reexport` and the log (`exported=N failed=0 moved=0`) | abe 0.4.0 (`eb7cd9d`) | e2e 8 "re-export after stop / configfs wipe / start" (same ports, markers read back); `summarizes_a_thin_pool` |
| 4 | The fio characterization wrote over the first GiB of the whole LV, destroying the GPT of non-raw volumes, and never trimmed, so a new thin volume already occupied 1 GiB | fio runs inside partition 1 (or the raw LV) over a region that stays within it, then `blkdiscard`s that region; `perf.trimmed` reports it; default `bs` documented as 4k | abe 0.4.0 (`2170bef`) | `fio_region_stays_inside_partition_one`, `parses_parted_partition_one`, `perf_reports_trimmed`; e2e 5 "trim" (`used_bytes` < 64 MiB, zeros read back), e2e 6 "GPT intact" |
| 5 | Every VM root was a full copy of the image; `/clone` existed but nothing could build a shared, frozen origin from an image | Base volumes: `GET /configure?base=<image>&bytes=` (tagged `abe_base`, idempotent per image), `GET /seal/<id>` (un-exports, tags `abe_sealed`; `GET /id` answers 409 afterwards, `/clone` refuses an unsealed base), `GET /version` listing `features` (`base`/`clone` only with a thin pool), `/volumes` entries carry `base`, `sealed`, `name`, `origin`, `layers` | abe 0.4.0 (`1d454c5`) | `validates_names_and_images`, `counts_layers`; e2e 0 "/version", e2e 7 "bases: make, fill, seal, clone twice, delete the base" |
| 6 | A retried create after a crash made a second volume; callers had no handle to find the first | `?name=` on `/configure` and `/clone` (tag `abe_name`): a second call with the same name returns the existing volume with `existing: true`; 400 outside `[A-Za-z0-9._+:-]{1,128}` | abe 0.4.0 (`36f4985`) | `validates_names_and_images`; e2e 4 "name idempotency: e2e-x once" |
| 7 | A tag write failing after `GET /id` had already exported the volume turned a successful attach into an error | That failure is a warning; the export stands | abe 0.4.0 (`3c8e871`) | e2e 1 (attach after the tag path) |
| 8 | `debian/abe.service` pointed `Documentation=` at the ABE repo under a misspelt GitHub account (the k was missing) | Link fixed; `tests/lint-links.sh` fails on the bad form | abe 0.4.0 (`0a7bbba`) | `tests/lint-links.sh` (clean) |
| 9 | The Debian archive already has a package called `abe` (a game); `apt install` could pick it | Package renamed `abe-block-exporter`, `Conflicts: abe`; `make install` evicts the game | abe 0.4.0 (`2907d0a`, `cb10127`) | `make deb && make install` on a host |

## xdebbie 0.15.0

| # | Issue | Fix | Version | Test that proves it |
|---|---|---|---|---|
| 10 | The client only knew `create` and `delete`, and nothing told it which ABE it was talking to | `abe::Client` gains `version` (404 → `Ok(None)`, cached 60 s, one `predates 0.4.0` warning per server), `create_named`, `export`, `base`, `seal`, `clone`, `utilization`, `has(feature)`; HTTP statuses travel in the error chain (`AbeHttpError`); names `vol-<name>` / `vol-<sha256>` and `vm-<uuid>-root`/`-data` within ABE's charset | 0.15.0 (`a1ee5fe`) | `client_speaks_abe_0_4_and_maps_statuses`, `a_pre_0_4_abe_has_no_features_and_is_cached`, `abe_names_follow_the_tag_charset`, `export_reply_carries_existing_and_tolerates_a_foreign_perf`; run.sh legacy L1 (no name/base/clone calls, warned once) |
| 11 | A root disk was a full image copy over the network for every VM | With `base`+`clone`: `qemu-img info` size check, `?base=` under a `KeyedLock<(server, image)>`, fill once + `blockdev --flushbufs` + disconnect + `/seal`, then `/clone/<base>?bytes=&name=vm-<uuid>-root`; `Disk.origin` records the base; `release_disks` deletes only the clone | 0.15.0 (`fcaf501`) | run.sh thin A3/A4a/A4b (base filled once, clone on the freed port), S1 (second VM: no fill, one more clone), A8 (base kept), crash C1 (ABE dies after `/seal`, VM still comes up with one fill) |
| 12 | Retried allocations after a crash could make a second volume | Data disks and standalone volumes are created with `?name=`; the first `Export` with `existing: true` is simply recorded | 0.15.0 (`a1ee5fe`, `fcaf501`) | run.sh S6 (five concurrent POSTs → one `/configure?name=vol-shared`), A2/A3 (failed first call leaks nothing) |
| 13 | A volume re-exported on another port after an ABE restart left the record (and `nvme connect`) pointing at the old one | `Hypervisor::refresh_endpoints` (`GET /id/<id>` for unconnected disks; on a change `nvme disconnect`, record update, warn `abe moved <id> from <old> to <new>; record updated`) runs before `prepare_disks`, before a cold `start` and before `attach_volume`; `VolPatch.endpoint` | 0.15.0 (`fcaf501`) | `moved_exports_are_refreshed_before_a_start_and_before_an_attach`; run.sh S8 (`/_move`, disconnect then connect on the new port, record follows) |
| 14 | The scheduler judged ABE VMs by the libvirt pool, so they landed on hosts with no ABE or no room | `Host.abeCapacity` from `/utilization` every tick (kept on error, unknown when stale), `Host::abe_fit` with reasons `no abe` / `abe capacity unknown` / `abe pool full` / `abe capacity`, config keys `abe_overcommit`, `abe_max_data_percent`, `abe_max_metadata_percent`, `place()` takes pending disks and the in-flight ledger, score is the fullest of cpu/mem/abe; `xdctl host ls` `ABE GiB` column | 0.15.0 (`a1ee5fe`, `fcaf501`) | `no_abe_and_unknown_capacity_are_distinct_reasons`, `thin_pool_checks_fill_then_provisioned_against_overcommit`, `thick_pool_uses_free_bytes_and_ignores_overcommit`, `used_fraction_feeds_the_score`, `staleness_is_by_seen`, `config_keys_take_snake_case`, `abe_vm_needs_an_abe_host_with_known_capacity`, `abe_vm_ignores_the_libvirt_pool_and_fits_by_abe_capacity`, `pending_disks_and_the_ledger_count_until_they_exist`, `spread_prefers_the_emptier_abe_pool`, `a_full_pool_is_rejected_even_with_promises_to_spare`, `local_vms_still_use_the_libvirt_pool`, `abe_capacity_comes_from_inventory_and_gates_placement`, `abe_vm_on_a_host_without_abe_stays_pending_with_a_reason`; run.sh capacity K1, noabe N1 |
| 15 | `POST /v1/volumes` used the first healthy host's ABE and could call ABE twice for one name; it had no answer for "nothing fits" | Per-name `KeyedLock`, store re-read under it, host with the most headroom (ties by name), bytes held in the ABE ledger by a drop guard until the record exists; 507 `no ABE host fits <bytes> bytes: …`, 503 when no host has an ABE | 0.15.0 (`fcaf501`) | `same_key_serializes_and_the_map_empties_afterwards`, `different_keys_run_in_parallel`, `a_waiter_keeps_the_entry_alive`; run.sh S6 (one 201, four 200), K1 (507), N1 (503) |
| 16 | `DELETE /v1/volumes` checked attachment on a snapshot and a failed ABE call left a record that looked attachable | One `store.update` does lookup / 409 / `deleting: true`; ABE failure answers 502 and keeps the flag; attach answers 409 `volume is being deleted`; the reconciler never attaches a deleting volume | 0.15.0 (`fcaf501`) | `a_deleting_volume_is_never_attached`; run.sh S7 (502 + `deleting: true`, attach 409, second DELETE 204) |
| 17 | New state fields (`Disk.origin`, `Volume.deleting`, `Host.abeCapacity` and the three limits) must not break a 0.14 `state.json` | All `#[serde(default)]`, `skip_serializing_if` where absent means the same thing | 0.15.0 (`a1ee5fe`) | `v0_14_state_loads_and_round_trips` |
| 18 | The ABE test fixtures spoke 0.3 and `tests/abe/run.sh` only covered the full-copy path | `fake-abe.py` implements the 0.4.0 surface plus `/_calls`, `/_move`, `FAKE_ABE_LEGACY`, `FAKE_ABE_PORT_MAX`, `FAKE_ABE_POOL_BYTES`, `FAKE_ABE_DIE_AFTER=seal`, `FAKE_ABE_DELETE_FAIL_ONCE`; the `qemu-img` stub answers `info --output=json`; `run.sh` runs five phases | 0.15.0 (`84213da`, `cab5aea`) | `tests/abe/run.sh`: 18 checks, all passing unprivileged |
| 19 | `docs/storage.md` and `CHANGELOG.md` linked ABE under the same misspelt account | Links fixed; `tests/lint-links.sh` wired into `make test` and `make lint` | 0.15.0 (`992b614`) | `tests/lint-links.sh` (clean) |
| 20 | Docs described 0.14 behaviour | `docs/storage.md` (base volumes, capacity accounting, endpoint refresh, what is not done, config reference), `docs/api.md` (507, 502 + `deleting`, 409), changelog | 0.15.0 (`284aaf0`, `cd4a227`) | review |

## abe-csi 0.3.5

| # | Issue | Fix | Version | Test that proves it |
|---|---|---|---|---|
| 21 | A full ABE surfaced on the PVC as a generic internal error | `scripts/abe-create-volume` exits 28 on xdebbied's 507 with the message as its last stderr line; `exec::ScriptError` carries the exit code and `From<ScriptError> for Status` maps 28 from that script to `ResourceExhausted(<message>)`, everything else stays `Internal`; README says so under Size | abe-csi 0.3.5 (`d099a61`) | `create_volume_exit_28_is_resource_exhausted_with_xdebbied_message`, `create_volume_other_exits_stay_internal`, `exit_28_from_other_scripts_is_not_special`, `last_line_tolerates_missing_prefix` |
| 22 | Repo links (section 5) | No hits in abe-csi; `tests/lint-links.sh` added and run by Drone before the build | abe-csi 0.3.5 (`d099a61`) | `tests/lint-links.sh` |

## Conflicts with the spec as written

- The spec was written against xdebbie `85993ce`; the branch starts at
  `560ad90` (0.15.0 already bumped, bootstrap/dhcp work in flight). Line
  numbers were ignored; nothing in `crates/xdebbied/src/bootstrap/` or
  `dhcp/` needed to change.
- `place()` takes one more argument than the spec sketched
  (`abe_inflight: &BTreeMap<String, u64>`, the volume ledger) so the
  scheduler stays pure; staleness is applied by
  `reconcile::hosts_for_placement` before every placement rather than
  inside `place()`.
- `Virsh.base_locks` is `Arc<KeyedLock<(String, String)>>` because `Virsh`
  is `Clone`.
- The spec's `delete_volume` wording ("one `store.update` does the lookup")
  is followed; a missing volume answers 204 (idempotent), not 404.
- `xdctl vm get` no longer exists at `560ad90` (`ls` has the alias);
  `tests/abe/run.sh` uses `vm describe`.
- A pre-0.4 ABE still answers `/utilization`, so `host ls` shows capacity
  for it too; the `-` column is tested on a host without `abe`.
- Not done, by the spec's own out-of-scope list: remote-host NVMe,
  migration of ABE VMs, base garbage collection.
